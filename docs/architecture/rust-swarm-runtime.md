# AF Swarm Runtime — Rust Architecture Plan

**Version:** 3.0 (post-TEE.fail revision)
**Date:** 2026-02-17
**Status:** Approved with conditions — incorporates team review consensus + TEE.fail security response
**Review sources:** [chief-architect-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md), [senku-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [lain-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/lain-review.md), [qa-engineer-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md), [senku-memory-deep-dive.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-memory-deep-dive.md), [depin-tee-architecture.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/revisions/depin-tee-architecture.md)
**TEE.fail review sources (2026-02-16):** [tee-fail-impact-analysis-senku.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-impact-analysis-chief.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [tee-fail-impact-analysis-lain.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-lain.md), [tee-fail-impact-analysis-qa.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md), [tee-fail-impact-analysis-argus.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-argus.md)

---

## 1. Vision

Replace the current TypeScript/PM2/Claude CLI agent system with a high-performance, model-agnostic Rust swarm runtime. The runtime must:

- Spawn 100+ concurrent agents (current limit: 4)
- Achieve <100ns local agent-to-agent latency (current: ~136us via NATS)
- Support any LLM (Anthropic, OpenAI, Google, local Ollama/vLLM/mistral.rs)
- Provide production security guardrails (sandboxing, injection defense, audit trail)
- Expose A2A protocol for external agent interoperability
- Deploy on Akash Network as a single binary

---

## 2. Current System (What We're Replacing)

```
User/Discord → NATS pub → [marketing-worker.ts / code-worker.ts]
                              ↓
                         execFile('claude', ['-p', '--agent', name])
                              ↓
                         Claude CLI subprocess (2-3s spawn time)
                              ↓
                         stdout captured → NATS pub → Discord
```

**Pain points:**
- 2-3 second cold start per agent (Node.js fork + Claude CLI boot)
- Locked to Claude models only (claude -p requires Anthropic)
- 4 concurrent agents max (PM2 + system resource limits)
- ~136us minimum latency for any agent-to-agent message (TCP through NATS broker)
- No sandboxing — agents run with full filesystem access
- No prompt injection defense
- No token budget enforcement
- No cross-organizational agent interop

---

## 3. Target Architecture

### 3.1 High-Level Data Flow

```
External Interfaces        API Gateway          Security        Orchestrator         Agents           Models
┌──────────────┐      ┌──────────────┐     ┌──────────┐    ┌──────────────┐   ┌──────────┐    ┌──────────┐
│ Browser WASM │─────►│              │     │Capability│    │              │   │          │    │Anthropic │
│ Discord      │─────►│ axum (REST)  │────►│Injection │───►│ DAG Planner  │──►│ Ractor   │───►│OpenAI    │
│ CLI          │─────►│ tonic (gRPC) │     │DSE Class.│    │ W&D Scheduler│   │ Actors   │───►│Gemini    │
│ A2A Protocol │─────►│ A2A (HTTP)   │     │Identity  │    │ LAMaS CPO    │   │          │    │Local/GGUF│
│ NATS (remote)│─────►│              │     │Budget    │    │ MAST Failure │   │          │    │          │
└──────────────┘      └──────────────┘     └──────────┘    └──────────────┘   └──────────┘    └──────────┘
                                                                                    │
                                                                    ┌───────────────┼───────────────┐
                                                                    ▼               ▼               ▼
                                                              ┌──────────┐   ┌──────────┐   ┌──────────┐
                                                              │  Memory  │   │  Tools   │   │Transport │
                                                              │ LanceDB  │   │ Wasmtime │   │ Flume    │
                                                              │ SYNAPSE  │   │ MCP      │   │ NATS     │
                                                              │ MemAgent │   │ FS/Shell │   │ bridge   │
                                                              └──────────┘   └──────────┘   └──────────┘
```

### 3.2 Hybrid Messaging Architecture

**The single biggest performance win.** 90% of agent traffic is local (same process). We use a tiered transport:

| Tier | Transport | Latency | When |
|------|-----------|---------|------|
| **L1: In-process** | Flume MPMC channels (native Rust types, no serialization) | ~80 ns | Agent A → Agent B (same runtime) |
| **L2: Topic fan-out** | `tokio::broadcast` | ~300 ns | Pub/sub within process |
| **L3: Cross-machine** | NATS (async-nats) with prost serialization | ~50-80 us | Remote agents, persistence, queue groups |

> **Review consensus (Senku, Chief Architect, Quinn):** Replaced `kanal` with `Flume` — zero `unsafe` code, proven at scale, eliminates the bus-factor-of-1 and past UB bug risks. The ~30ns latency difference is invisible when LLM inference takes 500ms-5s. L1 messages are native Rust types passed through Flume channels — no serialization occurs on the in-process path. See [senku-review.md §3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [chief-architect-review.md §4.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md), [qa-engineer-review.md §8.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md).

```
┌─────────────────────────────────────────────────┐
│              Agent Runtime Process               │
│                                                  │
│   Agent A ◄──Flume──► Message Router ◄──Flume──► Agent B  │
│                          │    │                  │
│              tokio::broadcast  │                 │
│              (topic fan-out)   │                 │
│                                │                 │
└────────────────────────────────┼─────────────────┘
                                 │
                          NATS Bridge Adapter
                     (only for cross-machine msgs)
                                 │
                          ┌──────┴──────┐
                          │ NATS Server │
                          └──────┬──────┘
                                 │
                    ┌────────────┴────────────┐
                    │                         │
             Machine B Runtime         Machine C Runtime
```

**Key rule:** The Message Router checks if the target agent is local. If yes → Flume (~80 ns). If no → NATS bridge (~50-80 us). The bridge is the ONLY component that touches the network.

### 3.3 A2A Protocol Gateway

Enables external agents (partner systems, marketplace, enterprise integrations) to interact with our swarm:

```
External Agent                        AF Swarm
     │                                    │
     │  GET /.well-known/agent.json       │
     │ ──────────────────────────────►    │  Discovery (Agent Cards)
     │  ◄──────────────────────────────   │  Returns capabilities, skills, auth requirements
     │                                    │
     │  POST /a2a/tasks/send              │
     │ ──────────────────────────────►    │  Task submission
     │                                    │  → Security check → Orchestrator → Agent actors
     │                                    │
     │  GET /a2a/tasks/{id} (SSE)         │
     │  ◄──────────────────────────────   │  Streaming results
     │                                    │
     │  POST /a2a/tasks/{id}/cancel       │
     │ ──────────────────────────────►    │  Cancellation → kill actor
```

**Agent Cards** are auto-generated from our existing TOML agent configs:
```json
{
  "name": "af-content-writer",
  "description": "Generates marketing copy, blog posts, and social content",
  "url": "https://agents.alternatefutures.ai",
  "capabilities": {
    "streaming": true,
    "pushNotifications": true
  },
  "skills": [
    {"id": "blog-writing", "name": "Blog Post Generation"},
    {"id": "social-copy", "name": "Social Media Copy"},
    {"id": "press-kit", "name": "Press Kit Creation"}
  ],
  "authentication": {
    "schemes": ["oauth2", "apiKey"]
  }
}
```

**Why A2A matters:**
1. Akash marketplace — other teams can discover and use our agents
2. Enterprise H2 — Salesforce, SAP, Atlassian are all A2A adopters
3. MCP + A2A = full interop stack (MCP for tools, A2A for agents)
4. Partner integrations (ElizaOS bots, external AI assistants)

---

## 4. Core Components

### 4.1 Actor Supervisor Tree (Ractor)

```
                     Root Supervisor
                    /       |        \
            Marketing    Code      Dynamic
            Supervisor   Supervisor  Supervisor
           /  |  \      / |  \       / | \
         BG  CW  GH   Sen Yus Lain  ... ...
```

- Erlang-style supervision: if a child crashes, supervisor restarts it
- Backpressure: parent limits concurrent children based on system resources
- Capability inheritance: children can only narrow parent's permissions, never widen
- Agent isolation: each actor has private state, communicates only via messages
- Semaphore-based rate limiter at actor message handler level as safety net for unbounded mailboxes (per [senku-review.md §1](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md))

### 4.2 Model Providers (via Rig framework)

```rust
// Unified trait — all providers implement this
// Wraps Rig behind our own abstraction to isolate from Rig's pre-1.0 API churn
trait ModelProvider: Send + Sync {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse>;
    async fn stream(&self, request: GenerateRequest) -> Result<Pin<Box<dyn Stream<Item = StreamChunk>>>>;
    fn supports_tools(&self) -> bool;
    fn model_id(&self) -> &str;
}
```

| Provider | Implementation | Use Case |
|----------|---------------|----------|
| Anthropic (Claude) | Rig's Anthropic client | Complex reasoning, long context |
| OpenAI-compatible | Rig's OpenAI client | Ollama, vLLM, Together, Groq |
| Google Gemini | Rig's Google client | Multimodal, large context |
| Local (mistral.rs) | FFI bridge (Qwen2.5-0.5B-Instruct Q4_K_M) | Offline routing/classification, 80-150+ tok/s CPU |

> **Review consensus (Senku, Chief Architect):** Rig is HIGH RISK (pre-1.0, API churn). Wrap in `ModelProvider` abstraction. Pin version. Have raw `reqwest` fallback plan. Zero Rig types leak into public API. See [chief-architect-review.md §7](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md), [senku-review.md §5](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md).

**SLM/LLM Router:** Classifies task complexity → routes simple tasks to local SLMs (10-30x cheaper, per NVIDIA research), complex tasks to cloud LLMs.

### 4.3 Memory Engine (3 Subsystems)

> **Review consensus (Senku, Chief Architect):** Simplified from 5 to 3 subsystems. SimpleMem and FadeMem are cut — MemAgent absorbs their responsibilities. See [senku-review.md §2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md) and [senku-memory-deep-dive.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-memory-deep-dive.md) for full rationale.

Three memory subsystems with clear, non-overlapping responsibilities:

| Component | Paper | What It Does |
|-----------|-------|-------------|
| **LanceDB vector store** | — | Embedding storage (via fastembed-rs / all-MiniLM-L6-v2, <1ms per doc), ANN similarity search, persistent disk-based store |
| **SYNAPSE graph memory** | SYNAPSE (2026) | Spreading activation replaces vector similarity; +23% accuracy, 95% token reduction |
| **MemAgent O(1) context** | MemAgent (ICLR 2026 oral) | Fixed-size working memory (32 slots); absorbs SimpleMem's compression (optional `summarize_on_evict` via local SLM) and FadeMem's decay (periodic LanceDB compaction job replaces real-time decay) |

**Data flow:**

```
Agent needs memory → MemAgent (fixed working context, 32 slots)
                        ↓ cache miss
                     SYNAPSE (spreading activation query)
                        ↓ retrieves from
                     LanceDB (vector store)
                        ↓ returns ranked results
                     MemAgent updates working context
```

**Two-tier eviction model (replaces SimpleMem + FadeMem):**

| Tier | Mechanism | Timescale | What It Does |
|------|-----------|-----------|-------------|
| **Tier 1: Working memory** | MemAgent eviction (LRU + relevance scoring) | Per-turn, real-time | Keeps agent context within 32-slot budget; `summarize_on_evict` optionally generates compressed summary as LanceDB metadata |
| **Tier 2: Long-term storage** | Compaction job (TTL + access count + relevance floor) | Every 6 hours, background | Prunes LanceDB vectors that are old AND rarely accessed AND low-relevance; never touches vectors in any active MemAgent buffer |

### 4.4 Orchestrator (PARL-lite)

Based on Kimi K2.5's PARL architecture (runtime layer only — no RL training loop):

| Component | Research Basis | Function |
|-----------|---------------|----------|
| DAG Task Planner | — | Parse prompt → dependency graph |
| Width-First Scheduler | W&D (2026) | Maximize parallel branches; width > depth |
| LAMaS Critical Path Optimizer | LAMaS (2026) | 38-46% critical path reduction (only open-source CPO) |
| Context Sharder | K2.5 PARL | Split large contexts across sub-agents |
| MAST Failure Detector | MAST (ICLR 2026) | 14 failure mode taxonomy, circuit breakers |

**Key insight from W&D paper:** Parallel tool calling (width scaling) is MORE impactful than using a smarter model (depth scaling). Our scheduler prioritizes spawning parallel agents over sequential deepening.

### 4.5 Tool System

> **Review consensus (Senku):** Replaced nsjail with Wasmtime WASM (primary) + bubblewrap (fallback). nsjail requires `CAP_SYS_ADMIN` or unprivileged user namespaces, which most Akash container providers do not grant. Wasmtime provides cross-platform sandboxing including macOS development. See [senku-review.md §8](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md).

| Tool | Implementation | Sandboxing |
|------|---------------|-----------|
| Filesystem (read/write/glob/grep) | Native Rust | **Wasmtime** (`wasi:filesystem` pre-opened dirs) |
| Shell executor | `tokio::process` | **bubblewrap** (runtime-detected) + seccomp fallback |
| Git | `git2-rs` | **bubblewrap** + scoped repos |
| HTTP fetch | `reqwest` | **Wasmtime** (`wasi:http` with host-enforced allowlist) |
| MCP client | JSON-RPC over stdio/SSE | **Wasmtime** (`wasi:http`); host manages MCP server lifecycle |
| KV cache | In-memory LRU | No sandbox needed (host-side) |

**Sandbox selection at startup:**
1. Wasmtime WASM for all WASI-compatible tools (~70-80% of invocations, <5us instantiation with pooling allocator)
2. Runtime-probe for unprivileged user namespace support (`unshare(CLONE_NEWUSER)`)
3. If available → bubblewrap with PID/mount/network namespace isolation
4. If unavailable → `prctl(PR_SET_SECCOMP)` with BPF syscall whitelist

---

## 5. Security Architecture

### 5.1 Six-Layer Security Stack

> **TEE.fail revision (v3.0, 2026-02-17):** The [TEE.fail](https://tee.fail/) paper (IEEE S&P 2026) demonstrated that Intel TDX and AMD SEV-SNP attestation can be forged using a sub-$1,000 DDR5 bus interposer. TEE (Layer 4) is downgraded from trust boundary to defense-in-depth layer. FHE (Layer 3) is promoted to primary security layer for RESTRICTED data. RA-TLS is replaced by multi-party attestation quorum. See [§5.6 TEE.fail Impact](#56-teefail-impact) for full analysis. Review consensus across all 5 team analyses: [tee-fail-impact-analysis-senku.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-impact-analysis-chief.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [tee-fail-impact-analysis-lain.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-lain.md), [tee-fail-impact-analysis-qa.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md), [tee-fail-impact-analysis-argus.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-argus.md).

The runtime implements defense-in-depth through six complementary security layers:

```
┌─────────────────────────────────────────────────────────────────────────┐
│              SIX-LAYER SECURITY STACK (post-TEE.fail v3.0)              │
│                                                                         │
│  Layer 6: DePIN (DECENTRALIZE — censorship resistance, NOT             │
│           confidentiality)                                               │
│  ├── Akash Network: general compute (Tier C — unaudited hardware)      │
│  ├── Phala Proof-of-Cloud: audited TEE compute (Tier B hardware)       │
│  ├── Hyperscaler VMs: controlled TEE compute (Tier A hardware)         │
│  └── Hardware trust tier determines routing, NOT provider name alone   │
│                                                                         │
│  Layer 5: WASM / Wasmtime (ISOLATE CODE — software layer, unaffected) │
│  ├── Capability-based sandboxing (wasi:filesystem, wasi:http)          │
│  ├── Per-tool memory limits via ResourceLimiter trait                   │
│  ├── Epoch-based CPU time interruption                                 │
│  └── Process spawning impossible (WASM architectural boundary)         │
│                                                                         │
│  Layer 4: TEE / Phala TDX (DEFENSE-IN-DEPTH — cost-of-attack          │
│           amplifier, NOT a trust boundary on uncontrolled hardware)     │
│  ├── Intel TDX: VM-level hardware isolation, AES-256-XTS memory       │
│  │   ⚠ TEE.fail: deterministic encryption enables pattern inference   │
│  │   via DDR5 bus interposition; attestation keys extractable (<$1K)  │
│  ├── AMD SEV-SNP: secondary TEE target (also affected by TEE.fail)    │
│  ├── Remote attestation: TD Quotes — ADVISORY signal only, not proof  │
│  │   of secure execution (forgeable via PCE key extraction)            │
│  ├── Sealed storage: secondary protection (FHE-encrypt before sealing)│
│  ├── Value on Tier A hardware: HIGH (insider attack required)          │
│  ├── Value on Tier B hardware: MODERATE (raises cost, not prevents)    │
│  ├── Value on Tier C hardware: LOW (operator IS potential attacker)    │
│  └── NEVER the sole protection for RESTRICTED data on DePIN            │
│                                                                         │
│  Layer 3: FHE / TFHE-rs (ENCRYPT PAYLOAD — PRIMARY SECURITY LAYER)    │
│  ├── Provably secure regardless of hardware trust                      │
│  ├── MANDATORY for RESTRICTED data on Tier B/C hardware                │
│  ├── Cross-tenant collaboration without decryption                     │
│  ├── Encrypted memory queries (~200ms for 10K vectors)                 │
│  ├── Privacy-preserving analytics aggregation                          │
│  └── 3 primitives: fhe_compare, fhe_aggregate, fhe_search             │
│                                                                         │
│  Layer 2: ZK Proofs / SP1 (VERIFY — math-based, unaffected)           │
│  ├── Agent action proofs without revealing inputs                      │
│  ├── Verifiable computation receipts                                   │
│  └── Marketplace billing verification                                  │
│                                                                         │
│  Layer 1: TLS 1.3 / rustls (TRANSPORT — modified)                      │
│  ├── In-process: none needed (L1 Flume channels)                       │
│  ├── NATS: mTLS 1.3 + NKey auth + 0-RTT resumption                    │
│  ├── External APIs: TLS 1.3 mandatory + HSTS + ALPN                   │
│  ├── LLM providers: TLS 1.3 + certificate pinning                     │
│  └── DePIN: mTLS 1.3 + multi-party attestation quorum (M-of-N        │
│       verifiers across independent providers — replaces single-source  │
│       RA-TLS which is demoted to one signal among many)               │
│                                                                         │
│  RESTRICTED data flow (Tier B/C):                                       │
│  TLS 1.3 → ZK verify → FHE encrypt → TEE* → WASM sandbox → DePIN    │
│  (transport)  (audit)  (REAL PROTECTION)  (*soft boundary) (code iso.) │
│                                                                         │
│  RESTRICTED secrets flow (Tier B/C):                                    │
│  Secret stays on Tier A proxy → only results sent to DePIN agent       │
└─────────────────────────────────────────────────────────────────────────┘
```

### 5.2 Security Guardrails (10 Components)

| ID | Component | Priority | What It Prevents |
|----|-----------|----------|-----------------|
| S1 | Capability-based permissions | P0 | Privilege escalation |
| S2 | Tool sandbox (Wasmtime WASM + bubblewrap fallback) | P0 | Filesystem/process escape |
| S3 | Prompt injection defense | P0 | Instruction hijacking via tool output |
| S4 | Secret management (Infisical) | P0 | API key exfiltration |
| S5 | Agent identity (Ed25519 + NKey) | P1 | Agent impersonation |
| S6 | Resource governor | P1 | Runaway cost/compute |
| S7 | Audit trail (hash chain) | P1 | Untracked actions |
| S8 | Model integrity (SHA-256) | P2 | Tampered model weights |
| S9 | Network egress control | P2 | Data exfiltration via HTTP |
| S10 | Data isolation + DSE taint propagation | P2 | Cross-agent memory access, data sensitivity violations |

**Prompt injection defense (S3) details:**
- All tool outputs tagged with trust level: `<tool-output trust="untrusted">...</tool-output>`
- Tag parsing done in Rust runtime BEFORE output enters the LLM prompt (prevents LLM-generated fake trust tags)
- Canary tokens in system prompts — if they appear in output, agent is compromised → kill
- Output validator scans for base64 payloads, redirect URLs, system file access
- Strict instruction hierarchy: system prompt > agent config > user prompt > tool output

**Resource governor (S6) details:**
```toml
[budget]
tokens_per_task = 100_000
tokens_per_day = 1_000_000
cost_per_day_usd = 5.00
max_concurrent_tools = 3
max_sub_agents = 0    # set > 0 to allow agent spawning
```

### 5.3 Data Sensitivity Engine (DSE)

The DSE classifies, tracks, and enforces data sensitivity across the entire task execution pipeline. It determines the minimum security tier for every piece of data and propagates taint through agent interactions.

#### 5.3.1 Three-Layer Classification

**Layer 1: Schema-Level TOML Declarations (static, human-authored)**

Agent and data model definitions declare field-level sensitivity at the schema level:

```toml
# agents/auth-billing-specialist.toml
[data_sensitivity]
default_tier = "INTERNAL"

[[data_sensitivity.fields]]
name = "api_key"
sensitivity = "RESTRICTED"
data_class = "CREDENTIAL"

[[data_sensitivity.fields]]
name = "email"
sensitivity = "SENSITIVE"
data_class = "PII_CONTACT"

[[data_sensitivity.fields]]
name = "display_name"
sensitivity = "PUBLIC"
data_class = "USER_PROFILE"

[[data_sensitivity.fields]]
name = "stripe_customer_id"
sensitivity = "RESTRICTED"
data_class = "FINANCIAL_ID"
```

**Sensitivity levels (4 tiers):**

| Level | Description | Min Transport | Allowed Agents |
|-------|------------|---------------|----------------|
| **PUBLIC** | Safe for external exposure | TLS 1.3 | Any agent |
| **INTERNAL** | Internal use only, no external leakage | TLS 1.3 | Authenticated agents |
| **SENSITIVE** | PII, credentials, financial data | mTLS 1.3 | Agents with explicit grant |
| **RESTRICTED** | Cryptographic keys, raw secrets, regulated data | mTLS 1.3 + E2E FHE mandatory (RA-TLS additive, not sufficient alone) | Named agents only, FHE-enforced; TEE as additive defense-in-depth (not a trust boundary — see [§5.6](#56-teefail-impact)) |

**Data classes (12 categories):**

| Class | Examples | Default Sensitivity |
|-------|---------|-------------------|
| `PUBLIC_CONTENT` | Blog posts, marketing copy | PUBLIC |
| `USER_PROFILE` | Display names, avatars | PUBLIC |
| `INTERNAL_CONFIG` | Agent configs, routing rules | INTERNAL |
| `INTERNAL_METRICS` | Usage counts, latency data | INTERNAL |
| `PII_CONTACT` | Email, phone, address | SENSITIVE |
| `PII_IDENTITY` | SSN, passport, government IDs | RESTRICTED |
| `FINANCIAL_ID` | Stripe IDs, billing records | SENSITIVE |
| `FINANCIAL_PCI` | Card numbers, CVVs | RESTRICTED |
| `CREDENTIAL` | API keys, tokens, passwords | RESTRICTED |
| `CRYPTO_KEY` | Private keys, seed phrases | RESTRICTED |
| `MODEL_WEIGHTS` | Proprietary model files | SENSITIVE |
| `AUDIT_LOG` | Security events, access logs | INTERNAL |

**Layer 2: Tool Manifest Declarations (per-tool, static)**

Each MCP tool or internal tool declares the sensitivity of its inputs and outputs:

```toml
# tools/infisical-read.toml
[tool]
name = "infisical-read"

[tool.sensitivity]
min_execution_tier = "RESTRICTED"

[[tool.sensitivity.inputs]]
name = "secret_path"
sensitivity = "INTERNAL"

[[tool.sensitivity.outputs]]
name = "secret_value"
sensitivity = "RESTRICTED"
data_class = "CREDENTIAL"

# tools/web-fetch.toml
[tool]
name = "web-fetch"

[tool.sensitivity]
min_execution_tier = "PUBLIC"

[[tool.sensitivity.inputs]]
name = "url"
sensitivity = "PUBLIC"

[[tool.sensitivity.outputs]]
name = "response_body"
sensitivity = "inherit"  # inherits from the task's current taint level
```

The `inherit` keyword propagates the parent task's taint level to the output, preventing sensitivity downgrade through tool chaining.

**Layer 3: Runtime Pattern Scanner (dynamic, real-time)**

Scans task context at runtime for sensitive content not caught by static declarations:

```rust
struct PatternScanner {
    patterns: Vec<SensitivityPattern>,
    co_occurrence_window: usize,  // tokens to scan around a match
}

struct SensitivityPattern {
    regex: Regex,
    data_class: DataClass,
    confidence_threshold: f64,  // 0.0 - 1.0
    min_sensitivity: SensitivityLevel,
}

// Example patterns:
// - API keys: r"(?i)(sk-|pk_|rk_)[a-zA-Z0-9]{20,}" → CREDENTIAL, confidence 0.95
// - SSN: r"\b\d{3}-\d{2}-\d{4}\b" → PII_IDENTITY, confidence 0.85
// - Credit cards: r"\b\d{4}[\s-]?\d{4}[\s-]?\d{4}[\s-]?\d{4}\b" → FINANCIAL_PCI, confidence 0.90
// - Private keys: r"-----BEGIN (RSA |EC )?PRIVATE KEY-----" → CRYPTO_KEY, confidence 0.99
// - High-entropy strings: entropy > 4.5 bits/char, length > 16 → CREDENTIAL, confidence 0.70

struct ScanResult {
    detected_class: DataClass,
    detected_sensitivity: SensitivityLevel,
    confidence: f64,
    matched_pattern: String,
    context_window: String,  // surrounding text for audit
}
```

**Co-occurrence windows:** The scanner checks for patterns within a configurable token window. A bare 9-digit number has low confidence for SSN. A 9-digit number within 50 tokens of "social security" or "SSN" has high confidence. This reduces false positives.

#### 5.3.2 Taint Propagation

Every task execution carries a `TaintContext` that propagates through the DAG:

```rust
struct TaintContext {
    current_tier: SensitivityLevel,
    taint_sources: Vec<TaintSource>,  // provenance trail
    declassify_gates: Vec<DeclassifyGate>,
}

struct TaintSource {
    origin: TaintOrigin,  // which layer detected it
    data_class: DataClass,
    sensitivity: SensitivityLevel,
    timestamp: Instant,
}

enum TaintOrigin {
    SchemaDeclaration(String),   // field name from TOML
    ToolManifest(String),        // tool name
    RuntimeScanner(ScanResult),  // pattern match
    ParentTask(TaskId),          // inherited from parent
    ManualOverride(AgentId),     // human or privileged agent override
}
```

**Taint only escalates, never silently downgrades.** When a sub-task accesses RESTRICTED data, all parent tasks in the DAG inherit that taint.

#### 5.3.3 Routing Decision

The final sensitivity tier for any task or data flow is the maximum across all sources:

```
effective_tier = max(
    agent.default_tier,        // from agent TOML config
    tool.min_execution_tier,   // from tool manifest
    scanner.detected_tier,     // from runtime pattern scan
    parent_task.taint_tier,    // inherited taint from parent DAG node
    manual_override            // human or security-agent override
)
```

This feeds directly into the Security Classifier (see [depin-tee-architecture.md §4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/revisions/depin-tee-architecture.md)), which routes tasks based on the effective tier. Post-TEE.fail, TEE routing is additive defense-in-depth — RESTRICTED data requires FHE encryption regardless of TEE availability, and SENSITIVE data requires ZK verification or WASM sandboxing as the minimum (TEE adds cost-of-attack amplification but is not the security boundary).

#### 5.3.4 Declassify Gates

Data can be declassified (sensitivity lowered) only through explicit declassify gates with re-scan verification:

```rust
struct DeclassifyGate {
    from_tier: SensitivityLevel,
    to_tier: SensitivityLevel,
    requires: DeclassifyRequirement,
}

enum DeclassifyRequirement {
    Redaction,           // PII removed/masked (e.g., email → j***@example.com)
    Aggregation,         // Individual records → aggregate statistics
    Encryption,          // Plaintext → FHE ciphertext
    Approval(AgentId),   // Named agent (e.g., chief-security-engineer) must approve
}
```

After declassification, the runtime re-scans the output to verify the sensitive data is actually gone. If the scanner still detects RESTRICTED patterns in data declassified to PUBLIC, the gate rejects the declassification.

### 5.4 FHE Integration (TFHE-rs)

Fully Homomorphic Encryption enables computation on encrypted data without decryption. This is NOT applied broadly — FHE operations are 1,000-10,000x slower than plaintext. We use it surgically for 5 specific use cases where privacy requirements outweigh latency costs.

#### 5.4.1 Five Surgical Use Cases

| # | Use Case | Why FHE | Latency Budget | Alternative Without FHE |
|---|----------|---------|---------------|------------------------|
| 1 | **Cross-tenant agent collaboration** | Tenant A's agent queries Tenant B's data without either seeing the other's plaintext | ~500ms per query | Impossible without a trusted intermediary |
| 2 | **Encrypted memory queries** | Search agent memory without exposing memory contents to the search infrastructure | ~200ms for 10K vectors | TEE-only (but TEE trusts the hardware vendor) |
| 3 | **Privacy-preserving analytics aggregation** | Compute aggregate metrics (avg response time, total token spend) across tenants without revealing individual values | ~100ms per aggregation | Differential privacy (weaker guarantees) |
| 4 | **Encrypted credential matching** | Verify "does this API key match?" without decrypting either the stored key or the presented key | ~50ms per comparison | Store hashed keys (weaker — vulnerable to rainbow tables for short keys) |
| 5 | **Verifiable marketplace billing** | Prove token usage and compute correct charges without revealing usage patterns | ~300ms per billing cycle | Trust the billing service (centralized trust) |

#### 5.4.2 Three FHE Primitives

All FHE operations are built from three primitives implemented via TFHE-rs:

```rust
/// Compare two encrypted values, return encrypted boolean
/// Use case: credential matching, threshold checks
async fn fhe_compare(
    a: &FheUint64,
    b: &FheUint64,
    op: CompareOp,  // Eq, Lt, Gt, Lte, Gte
) -> FheBool;

/// Aggregate encrypted values (sum, count, mean)
/// Use case: analytics, billing totals
async fn fhe_aggregate(
    values: &[FheUint64],
    op: AggregateOp,  // Sum, Count, Mean, Max, Min
) -> FheUint64;

/// Search encrypted vectors for nearest neighbor
/// Use case: encrypted memory queries
/// Returns indices (encrypted) of top-k matches
async fn fhe_search(
    query: &FheVector,
    corpus: &[FheVector],
    k: usize,
) -> Vec<FheUint32>;
```

#### 5.4.3 Performance and GPU Acceleration

| Operation | CPU (single core) | GPU (CUDA, TFHE-rs) | Target for AF |
|-----------|------------------|---------------------|---------------|
| `fhe_compare` (64-bit) | ~50ms | ~5ms | GPU when available |
| `fhe_aggregate` (1K values) | ~2s | ~100ms | GPU required |
| `fhe_search` (10K vectors) | ~30s | ~200ms | GPU required |
| Key generation | ~5s | ~500ms | One-time cost per tenant |

GPU acceleration via CUDA is available through TFHE-rs's `cuda` feature flag. On DePIN providers without GPU (standard Akash containers), FHE operations fall back to CPU — acceptable for `fhe_compare` (~50ms) but impractical for `fhe_search` (~30s). Route FHE-heavy workloads to io.net or Aethir GPU providers.

**Build estimate:** 22 agent-hours for the FHE subsystem (3 primitives + key management + GPU dispatch + integration tests).

### 5.5 TLS 1.3 Transport Encryption

All network communication uses TLS 1.3 via `rustls` (pure Rust, no OpenSSL dependency). Transport requirements are layered by communication path:

#### 5.5.1 Five Transport Layers

| Layer | Path | Protocol | Details |
|-------|------|----------|---------|
| **L1: In-process** | Flume channels (agent ↔ agent, same runtime) | None needed | Native Rust types in shared memory. No network, no serialization, no encryption overhead. |
| **L2: NATS** | Agent ↔ agent (cross-machine), JetStream persistence | mTLS 1.3 + NKey + 0-RTT resumption | Both client and server present certificates. NKey (Ed25519) provides agent-level identity (aligns with S5). TLS 1.3 0-RTT resumption reduces reconnection overhead for persistent NATS connections. |
| **L3: External APIs** | API gateway ↔ browser/CLI/A2A clients | TLS 1.3 mandatory + HSTS + ALPN | `Strict-Transport-Security: max-age=31536000; includeSubDomains`. ALPN negotiation for HTTP/2 multiplexing. Certificate via Let's Encrypt or Cloudflare origin certs. |
| **L4: Outbound LLM** | Runtime ↔ Anthropic / OpenAI / Google APIs | TLS 1.3 + certificate pinning | Pin leaf certificates for `api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com`. Pinning prevents MITM even if a CA is compromised. Rotation handled via config hot-reload. |
| **L5: DePIN** | Runtime ↔ Phala TEE enclaves | mTLS 1.3 + multi-party attestation quorum (M-of-N verifiers) | ~~RA-TLS sole verification~~ **DEPRECATED** — TEE.fail enables attestation forgery via PCE key extraction. Replaced with M-of-N multi-party attestation: K independent verifier nodes on different providers/hardware vendors each challenge the target node; M-of-N must agree before trust is established. RA-TLS remains as one signal in the composite trust assessment, not the sole verification. See [tee-fail-impact-analysis-chief.md §5.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [tee-fail-impact-analysis-senku.md §5.5](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md). |

#### 5.5.2 Minimum Transport per Sensitivity Level

| Data Sensitivity | Minimum Transport | Rationale |
|-----------------|------------------|-----------|
| **PUBLIC** | TLS 1.3 (L3) | Standard web encryption |
| **INTERNAL** | TLS 1.3 (L3) | Same as PUBLIC — transport is not the differentiation point for INTERNAL |
| **SENSITIVE** | mTLS 1.3 (L2/L5) | Client authentication required — both sides prove identity |
| **RESTRICTED** | mTLS 1.3 + end-to-end FHE mandatory | FHE payload encryption required — transport alone is insufficient. TEE/RA-TLS is additive defense-in-depth but NOT sufficient as sole protection (TEE.fail enables attestation forgery). On Tier A (controlled) hardware, TEE provides high value; on Tier B/C (DePIN), FHE is the security boundary. |

These minimums are enforced by the DSE routing decision. If a task's effective tier is RESTRICTED and the target agent is reachable only via standard TLS, the task is rejected.

#### 5.5.3 Implementation

```rust
// rustls configuration for each layer
fn build_tls_config(layer: TransportLayer) -> rustls::ClientConfig {
    let mut config = rustls::ClientConfig::builder()
        .with_protocol_versions(&[&rustls::version::TLS13])  // TLS 1.3 only
        .with_root_certificates(root_store)
        .with_no_client_auth();  // overridden for mTLS layers

    match layer {
        TransportLayer::Nats => {
            // L2: mTLS — load client cert + key for NKey identity
            config.client_auth_cert_resolver = Arc::new(NKeyResolver::new());
            config.enable_early_data = true;  // 0-RTT resumption
        }
        TransportLayer::LlmProvider(provider) => {
            // L4: Certificate pinning
            config.dangerous()
                .set_certificate_verifier(Arc::new(PinningVerifier::new(
                    provider.pinned_certs()
                )));
        }
        TransportLayer::DePinTee => {
            // L5: Multi-party attestation quorum (post-TEE.fail)
            // DEPRECATED: Single-source RA-TLS verification is insufficient —
            // TEE.fail (IEEE S&P 2026) demonstrated PCE key extraction enabling
            // attestation forgery that passes DCAP at "UpToDate" trust level.
            // Replaced with M-of-N multi-party attestation from independent
            // hardware verifiers across different providers and TEE vendors.
            // See: tee-fail-impact-analysis-chief.md §5.2
            config.dangerous()
                .set_certificate_verifier(Arc::new(
                    MultiPartyAttestationVerifier::new(
                        quorum_config(),       // M-of-N verifier threshold
                        expected_measurements(),
                    )
                ));
        }
        _ => {}  // L3: standard TLS 1.3, no extra config
    }
    config
}
```

### 5.6 TEE.fail Impact

> **Added in v3.0 (2026-02-17).** This section documents the impact of the [TEE.fail](https://tee.fail/) vulnerability (IEEE S&P 2026) on our security architecture and the architectural changes made in response. Based on unanimous consensus across 5 team reviews: Senku (Technical Lead), Chief Security Architect, Lain (Network/Distributed Systems), Quinn (QA), and Argus (Security).

#### 5.6.1 Vulnerability Summary

TEE.fail (Chuang et al., Georgia Tech/Purdue) demonstrated that a sub-$1,000 DDR5 bus interposer can:

1. **Extract ECDSA attestation private keys** from Intel's Provisioning Certification Enclave via deterministic encryption pattern analysis and nonce recovery
2. **Forge valid TDX and SGX attestation quotes** that pass Intel's DCAP Quote Verification Library at the highest trust level ("UpToDate")
3. **Affect every TEE platform**: Intel TDX, AMD SEV-SNP, NVIDIA Confidential Computing (H100/H200/B100/B200)
4. **Operate from a portable apparatus** fitting in a 17-inch briefcase — hobbyist-grade, not nation-state

The root cause is **deterministic AES-XTS memory encryption**: identical plaintext at the same physical address produces identical ciphertext. This is a design decision, not a bug — both Intel and AMD classify it as "out of scope" with no patches incoming. The vulnerability will persist indefinitely on current-generation hardware.

**Real-world impact demonstrated:** Secret Network lost its entire confidentiality guarantee (consensus seed extracted). Flashbots BuilderNet was vulnerable to MEV theft (config secrets obtained via forged attestation). Phala's dstack framework was shown running non-confidential VMs while forging all attestation data.

Sources: [TEE.fail paper](https://tee.fail/files/paper.pdf), [Phala DDR5 response](https://phala.com/posts/phala-ddr5), [Secret Network response](https://scrt.network/blog/secret-labs-response-to-tee-fail-ddr5-vulnerability), [BleepingComputer](https://www.bleepingcomputer.com/news/security/teefail-attack-breaks-confidential-computing-on-intel-amd-nvidia-cpus/)

#### 5.6.2 What Changed in Our Architecture

| Component | Before (v2.0) | After (v3.0) | Source |
|-----------|--------------|-------------|--------|
| **Layer 4 (TEE) role** | Trust boundary — "ENCRYPT COMPUTE" | Defense-in-depth — cost-of-attack amplifier, NOT a trust boundary on uncontrolled hardware | All 5 reviews unanimous |
| **RESTRICTED data tier** | "mTLS 1.3 + RA-TLS **OR** E2E FHE" | "mTLS 1.3 + E2E FHE **mandatory**" — FHE is the security boundary, TEE is additive | [Chief §4.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [QA §3.4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md) |
| **RA-TLS (L5 transport)** | Sole verification — "proving the peer runs inside a genuine TEE" | Demoted to one signal in multi-party attestation quorum (M-of-N verifiers across independent providers) | [Chief §5.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [Lain §1.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-lain.md) |
| **FHE (Layer 3) role** | Future enhancement, 5 surgical use cases | PRIMARY security layer for RESTRICTED data — mandatory on Tier B/C hardware | [Senku §3.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [QA §3.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md) |
| **FHE build phase** | Phase 14 (deferred, post-MVP) | Phase 6 (accelerated, pre-milestone) | [Senku §7.1](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [Chief §8.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md) |
| **DePIN hardware trust** | Binary (TEE or sandbox) | Three-tier classification: Tier A (controlled), Tier B (audited DePIN), Tier C (unaudited DePIN) | [Senku §5.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [Argus §6](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-argus.md) |
| **Secret provisioning** | Infisical authenticates via attestation proof, secrets delivered to TEE memory | Secret Proxy pattern — secrets stay on Tier A hardware, DePIN agents receive only FHE-encrypted data or proxied results | [Senku §5.4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [Lain §4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-lain.md) |

#### 5.6.3 Hardware Trust Tiers

```
┌─────────────────────────────────────────────────────────────┐
│                  HARDWARE TRUST TIERS                         │
│                                                               │
│  Tier A: CONTROLLED HARDWARE                                 │
│  ├── Our own servers, hyperscaler VMs (GCP, Azure, AWS)      │
│  ├── Physical security: data center operator guarantee        │
│  ├── TEE value: HIGH (interposer attack requires insider)    │
│  └── Acceptable for: RESTRICTED data with TEE                │
│                                                               │
│  Tier B: AUDITED DePIN                                       │
│  ├── Phala Proof-of-Cloud nodes in verified data centers     │
│  ├── Physical security: third-party audit, not our control   │
│  ├── TEE value: MODERATE (raises attack cost, not prevents)  │
│  └── Acceptable for: SENSITIVE data with TEE + additional    │
│       controls; RESTRICTED only with FHE mandatory           │
│                                                               │
│  Tier C: UNAUDITED DePIN                                     │
│  ├── Community DePIN nodes, Akash standard providers         │
│  ├── Physical security: NONE guaranteed                      │
│  ├── TEE value: LOW (attacker IS the operator)               │
│  └── Acceptable for: PUBLIC/INTERNAL data only, OR           │
│       RESTRICTED data ONLY if FHE-wrapped                    │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

#### 5.6.4 Revised Security Routing

```
RESTRICTED + Tier A hardware → mTLS 1.3 + RA-TLS + TEE (acceptable)
RESTRICTED + Tier B hardware → mTLS 1.3 + FHE mandatory (TEE as bonus)
RESTRICTED + Tier C hardware → mTLS 1.3 + FHE mandatory (TEE not trusted)
RESTRICTED + no FHE possible → REJECT (do not process on Tier B/C)

Secrets requiring plaintext use → Tier A only (Secret Proxy pattern)
```

#### 5.6.5 What Still Works

| Layer | Status Post-TEE.fail | Rationale |
|-------|---------------------|-----------|
| Layer 1: TLS 1.3 | **INTACT** (4 of 5 sub-layers) | Transport encryption is independent of TEE. L5 RA-TLS is the one affected sub-layer. |
| Layer 2: ZK Proofs | **INTACT** | Mathematical guarantee — proves computation correctness regardless of where it ran. |
| Layer 3: FHE | **INTACT — promoted to primary** | Data stays encrypted during compute. Even a fully compromised TEE sees only FHE ciphertext. |
| Layer 4: TEE | **DOWNGRADED** | Still protects against software-only attacks. Against physical access on DePIN: raises cost but doesn't prevent attack. |
| Layer 5: WASM | **INTACT** | Software sandbox independent of hardware trust. |
| Layer 6: DePIN | **TRUST MODEL ADJUSTED** | Decentralization still provides censorship resistance. Confidentiality guarantee of TEE-based DePIN is void — compensated by FHE. |

#### 5.6.6 Competitive Advantage

Most TEE-dependent DePIN projects used TEE as their **sole** trust mechanism. Our six-layer stack was designed for defense-in-depth — the architectural philosophy that any single layer can be compromised without breaking overall security. TEE.fail validates this approach:

- **Secret Network**: TEE was their only protection → catastrophic (consensus seed extracted)
- **BuilderNet**: TEE for credential isolation → compromised (validator wallet access)
- **AF (us)**: TEE was one of six layers → degraded but recoverable via FHE promotion

The key philosophical shift: **"We trust mathematics (FHE, ZK), not hardware. TEE is a useful cost-of-attack amplifier, but the real security boundaries are cryptographic."** This is analogous to the industry shift from "firewalls are the perimeter" to "zero trust."

---

## 6. Config Hot-Reload

> **Review consensus (Senku, Chief Architect):** Hot reload was a critical gap in v1.0. For a system running 100+ agents, requiring restart for config changes is unacceptable. See [senku-review.md §7](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [chief-architect-review.md §3.4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md).

### Three-Tier Hot Reload

**Tier 1 (implement first): Config-only hot reload via ArcSwap + notify**

| Component | Crate | Purpose |
|-----------|-------|---------|
| File watching | `notify` + `notify-debouncer-mini` | Detect agent config file changes |
| Atomic config swap | `arc-swap` | Lock-free config distribution (~55ns read, vs ~352ns for `parking_lot::RwLock`) |
| Signal reload | `tokio::signal` (SIGHUP) | Manual trigger |
| Actor messaging | Ractor (built-in) | `Reconfigure(AgentConfig)` message variant |

**Flow:**
1. File watcher detects change to `agents/market-intel.toml`
2. Parse and validate new config — if invalid, log error, keep old config
3. `ArcSwap::store(Arc::new(new_config))`
4. Look up agent via Ractor registry: `ractor::registry::where_is("agent-market-intel")`
5. Send `Reconfigure` message to agent actor
6. Agent applies new config on next handler invocation
7. In-flight requests complete with old config snapshot (Arc keeps it alive)

**Model provider hot-swap:** Use `ArcSwap<Box<dyn ModelProvider>>`. Current streaming response completes with old provider; next request picks up new provider.

**Tier 2 (implement when needed):** WASM tool plugins — hot-swappable tool implementations via Wasmtime.

**Tier 3 (implement last):** Graceful binary restart via `shellflip` crate for socket-inheriting process replacement.

---

## 7. Build Phases & Agent-Time Estimates (Revised 15-Phase Plan, post-TEE.fail)

> **Review consensus (Chief Architect):** Original 53.75h estimate is unrealistic. Revised to 400-600 agent-hours for MVP. Key changes: agent definitions moved early, security phased (P0 first), memory simplified, A2A deferred, testing continuous. See [chief-architect-review.md §1.1, §6](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md).
>
> **TEE.fail revision (v3.0):** FHE integration moved from Phase 14 to Phase 6 — FHE is now the primary security layer for RESTRICTED data and must be available before any RESTRICTED workloads deploy on DePIN. New Phase 6b added for TEE.fail mitigations (Secret Proxy, hardware trust tiers, multi-party attestation). Total revised estimate: 516-816 agent-hours. See [tee-fail-impact-analysis-senku.md §7](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-impact-analysis-chief.md §8.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md).

All times are agent execution time (Claude Code agent sessions). Realistic estimates account for Rust learning curve, async lifetime complexity, and integration friction.

| Phase | Components | Agent-Hours | Notes |
|-------|-----------|------------|-------|
| **0: Scaffolding** | Cargo workspace, types, errors, agent config schema design | 8-12h | Includes TOML agent format with `[data_sensitivity]` and `[a2a]` sections |
| **1: Core Runtime** | Ractor actors, single model provider, basic config loading | 30-45h | `ModelProvider` trait wrapping Rig, ArcSwap for config |
| **2: Agent Definitions** | Config format, prompt assembler, load 3 pilot agents | 15-25h | Agents moved early so all subsequent phases are testable |
| **3: Messaging** | Flume local bus, topic router, NATS bridge adapter (prost) | 10-15h | Replaces kanal; prost for NATS, owned types for in-process |
| **4: Basic Security** | S1 (capabilities) + S4 (Infisical secrets) + S6 (budgets) + DSE layer 1 + hardware trust tier classification | 30-50h | P0 guardrails only; DSE schema declarations; **TEE.fail: +5-10h for hardware trust tier integration** |
| **5: Basic API** | REST + WebSocket, Discord adapter | 20-30h | Essential transport; A2A deferred |
| **6: Memory + FHE** | LanceDB + SYNAPSE + MemAgent (3 subsystems), fastembed-rs, migration script, **FHE primitives (TFHE-rs)** | 52-72h | Simplified from 5→3 per review consensus; **TEE.fail: FHE moved here from Phase 14 (+22h) — fhe_compare, fhe_aggregate, fhe_search, key management, GPU dispatch** |
| **6b: TEE.fail Mitigations** | Secret Proxy (Tier A), multi-party attestation quorum, canary verification, FHE mandatory enforcement in DSE routing | 48-68h | **NEW PHASE (TEE.fail).** Secret Proxy: 20h. Multi-party attestation: 20-30h. Canary system: 12h. Behavioral anomaly detection: 15h. See [tee-fail-impact-analysis-senku.md §7.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md) |
| **7: Tool System** | Registry, FS, shell, MCP client, HTTP, git, KV cache, Wasmtime sandbox | 25-40h | Wasmtime primary + bubblewrap fallback |
| **8: Orchestrator** | Basic DAG planner (no LAMaS/MAST/W&D yet) | 20-30h | Simple scheduler first, advanced components Phase 13 |
| **9: Observability** | Logging (tracing), metrics (prometheus), health, cost tracker, OpenTelemetry tracing across NATS | 15-25h | Includes distributed tracing per [qa-engineer-review.md FINDING-29](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md) |
| **10: Migration** | Parallel run with TS system, 3 pilot agents, JetStream + shadow mode validation | 20-30h | Milestone: production with 3 agents |
| --- | **MILESTONE: Production with 3 agents** | **314-462h** | |
| **11: Remaining Agents** | Port remaining 14 agents, behavioral validation | 25-40h | Golden-file test suite for personality preservation |
| **12: Advanced Security** | S2 (Wasmtime sandbox hardening), S3 (injection defense), S5 (identity), S7-S10, DSE layers 2-3, TLS 1.3 layered transport, continuous attestation monitoring | 55-80h | Full DSE with taint propagation, runtime scanner; **TEE.fail: +15-20h for continuous attestation and app-layer NATS encryption** |
| **13: Advanced Orchestrator** | W&D scheduler, LAMaS CPO, MAST failure detector (includes FM-15 through FM-18 per QA review) | 25-40h | Layer research components one at a time with A/B testing |
| **14: A2A Gateway** | Full A2A protocol (11 operations), push notifications, rate limiting, hardware diversity execution, ZK verification for deterministic ops | 50-70h | Only if market demand confirmed; **TEE.fail: hardware diversity +10-15h, ZK verification +15-25h** |
| **TOTAL** | | **516-816h** | **+116-216h from TEE.fail mitigations** |

### Critical Path

```
Phase 0 → Phase 1 → Phase 2 → Phase 3 → Phase 4 (Security P0) → Phase 10 (Migration)
                                            ↗ Phase 5 (API)
                                            ↗ Phase 6 (Memory + FHE)
                                            ↗ Phase 6b (TEE.fail Mitigations) — blocks RESTRICTED on DePIN
                                            ↗ Phase 7 (Tools)
                                            ↗ Phase 8 (Orchestrator)
                                            ↗ Phase 9 (Observability)
```

> **TEE.fail note:** Phase 6 (FHE) and Phase 6b (TEE.fail mitigations) are on the critical path for any RESTRICTED data processing on DePIN. No RESTRICTED workloads may deploy to Tier B/C hardware until FHE mandatory enforcement and the Secret Proxy are operational.

**Testing is continuous — not a separate phase.** Every phase includes unit and integration tests. Benchmarks run on every PR merge via `criterion` + `cargo-criterion`.

---

## 8. Technology Stack (Revised)

> **Review consensus changes highlighted inline.**

| Layer | Technology | Why |
|-------|-----------|-----|
| Language | Rust (2024 edition) | Raw speed, memory safety, zero-cost abstractions |
| Async runtime | tokio | Industry standard, mature ecosystem |
| Actor framework | Ractor | Erlang-style supervision, message passing |
| LLM client | Rig (wrapped in `ModelProvider` trait) | Multi-provider (Anthropic, OpenAI, Google), streaming, tool use. **Pin version, have reqwest fallback.** |
| Local inference | mistral.rs (FFI) | Qwen2.5-0.5B-Instruct Q4_K_M for routing/classification |
| Embedding inference | **fastembed-rs (ort)** | ONNX embedding models (all-MiniLM-L6-v2), <1ms/doc, 3x less RAM than mistral.rs embeddings |
| Vector store | LanceDB | Rust-native, embedded, ANN search |
| Messaging (local) | **Flume** + tokio::broadcast | **Zero unsafe code**, proven MPMC, ~80 ns (replaces kanal) |
| Messaging (remote) | NATS (async-nats) | Pub/sub, queue groups, JetStream, ~50-80 us from Rust |
| HTTP framework | axum | Fast, tower middleware, tonic integration |
| gRPC | tonic | Protobuf codegen, bidirectional streaming |
| Git | git2-rs | libgit2 bindings, no subprocess needed |
| HTTP client | reqwest | Mature, TLS, connection pooling |
| Sandbox | **Wasmtime** (primary) + **bubblewrap** (fallback) | WASM capability-based isolation (cross-platform); bubblewrap for Linux namespace tools |
| Logging | tracing + tracing-subscriber + **tracing-opentelemetry** | Structured, span trees, **distributed tracing across NATS** |
| Metrics | prometheus crate | Standard export format |
| Serialization | **prost** (NATS/persistence) + owned types (in-process) + serde_json (debug) | Schema evolution via protobuf, no serialization on L1 hot path |
| Config hot reload | **ArcSwap + notify** | Lock-free config swap (~55ns read), file watching |
| TLS | **rustls** | Pure Rust TLS 1.3, no OpenSSL dependency |
| FHE | **TFHE-rs** | Fully homomorphic encryption, CUDA GPU acceleration |
| Build | cargo + cargo-nextest | Parallel test execution |
| License | **Apache 2.0** (core runtime) | Permissive, compatible with enterprise adoption |

---

## 9. Performance Targets

| Metric | Current (TS) | Target (Rust) | Improvement |
|--------|-------------|--------------|-------------|
| Agent spawn | 2-3s | <3ms | ~1000x |
| Local agent-to-agent latency | ~136 us (NATS) | ~80 ns (Flume) | ~1700x |
| Memory lookup | ~50ms (file parse) | <1ms (LanceDB ANN) | ~50x |
| Tool dispatch | ~100ms (subprocess) | <5ms (in-process) | ~20x |
| Concurrent agents | 4 | 100+ | ~25x |
| Token overhead per agent | ~2000 tokens | ~200 tokens (compressed) | ~10x |
| Cold start (full runtime) | ~5s | <50ms | ~100x |

> **Note (Chief Architect):** The ~80ns figure is Flume channel primitive latency. End-to-end local message latency (including routing, actor mailbox processing) will be ~500ns-2us — still a massive improvement over 136us NATS. See [chief-architect-review.md §1.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md).

---

## 10. Migration Strategy

### Phase A: Parallel Run (Week 1-2)
- Rust runtime runs alongside existing TS workers
- Both subscribe to same NATS subjects (queue groups ensure no duplication)
- **Use JetStream with explicit ack** (not core pub/sub) for crash resilience
- **Shadow mode:** both systems process every message; only TS output goes to Discord; Rust output goes to validation queue for comparison
- Gradually shift traffic: 10% → 50% → 100% to Rust

### Phase B: Feature Parity (Week 2-3)
- All 18 agents ported to Rust agent configs
- **Golden-file behavioral test suite:** 50-100 representative inputs per agent, LLM-as-judge for semantic similarity, manual spot-check for personality preservation
- Discord adapter replaces Node.js bots
- A2A gateway live for external access

### Phase C: Cutover (Week 3-4)
- Decommission TS workers and PM2
- **TS Docker images preserved for 2 weeks post-cutover** for instant rollback
- Single Rust binary on Akash
- NATS remains for external distribution + JetStream persistence

---

## 11. Open Questions for Review

1. **UX:** How should external developers interact with our agents via A2A? What does the developer experience look like?
2. **UX:** Should we build a web dashboard for the swarm runtime (agent status, task progress, cost tracking)?
3. **Gaps:** Are there agent communication patterns we haven't considered? (Consensus, voting, negotiation?)
4. **Gaps:** Do we need agent-to-agent trust levels beyond the flat capability model?
5. ~~**Security:** Is nsjail sufficient for production sandboxing, or do we need Firecracker microVMs?~~ **RESOLVED:** Wasmtime WASM (primary) + bubblewrap (fallback). See §4.5.
6. ~~**Performance:** Should we consider io_uring for filesystem tools instead of standard async I/O?~~ **RESOLVED:** Premature — io_uring helps high-frequency filesystem ops, but agent tools are I/O-bound on network latency. Revisit if benchmarks show filesystem dispatch as bottleneck.
7. **Market:** How does this position us competitively? Is "model-agnostic agent runtime" a sellable product?
8. **Market:** Should A2A support be a free feature or a paid enterprise tier?
9. **DevEx:** What SDK/CLI should external developers use to deploy agents on our runtime?
10. ~~**Memory:** Should agent memory be portable/exportable (agents can move between runtimes)?~~ **RESOLVED:** Yes — define `VectorStore` trait, export via LanceDB snapshot (AF-to-AF) or MemAgent summary format (cross-runtime). See [senku-review.md §4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md).

---

## 12. Team Review Consensus Log

Key decisions made during the 2026-02-15 review cycle, with references:

| Decision | Old | New | Source |
|----------|-----|-----|--------|
| Local channels | kanal (pre-release, past UB bug, bus factor 1) | **Flume** (zero unsafe, proven) | [chief-architect-review.md §4.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md), [senku-review.md §3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [qa-engineer-review.md FINDING-34](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md) |
| Sandboxing | nsjail (requires CAP_SYS_ADMIN, Linux-only) | **Wasmtime** (cross-platform, <5us instantiation) + **bubblewrap** fallback | [senku-review.md §8](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [chief-architect-review.md §4.5](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md) |
| Serialization | serde + MessagePack everywhere | **prost** (NATS/persistence), **owned types** (in-process), **serde_json** (debug) | [senku-review.md §3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md) |
| Memory subsystems | 5 (LanceDB, SYNAPSE, SimpleMem, FadeMem, MemAgent) | **3** (LanceDB, SYNAPSE, MemAgent) — MemAgent absorbs SimpleMem + FadeMem | [senku-review.md §2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [senku-memory-deep-dive.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-memory-deep-dive.md), [chief-architect-review.md §4.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md) |
| Embeddings | Unspecified | **fastembed-rs** (ort/ONNX, <1ms/doc, 80MB RAM) | [senku-review.md §5](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md) |
| Hot reload | Not addressed | **ArcSwap + notify** + Ractor reconfigure messages | [senku-review.md §7](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [chief-architect-review.md §3.4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md) |
| Build estimate | 53.75h | **400-600h** (14-phase plan) | [chief-architect-review.md §1.1](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md) |
| License | Unspecified | **Apache 2.0** for core | Team consensus |
| Local inference model | Unspecified | **Qwen2.5-0.5B-Instruct Q4_K_M** via mistral.rs FFI | [senku-review.md §5](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md) |
| Distributed tracing | Not addressed | **OpenTelemetry** (W3C traceparent) propagated in NATS headers | [qa-engineer-review.md FINDING-29](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md) |

**TEE.fail decisions (2026-02-16/17 review cycle, 5 team reviews):**

| Decision | Old | New | Source |
|----------|-----|-----|--------|
| TEE role | Trust boundary ("ENCRYPT COMPUTE") | **Defense-in-depth** — cost-of-attack amplifier, NOT a trust boundary on uncontrolled hardware | All 5 TEE.fail reviews unanimous |
| RESTRICTED data routing | "RA-TLS **OR** E2E FHE" | "E2E FHE **mandatory**" — RA-TLS is additive, not sufficient alone | [tee-fail-chief §4.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [tee-fail-qa §3.4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md) |
| RA-TLS verification | Sole trust anchor for DePIN transport | **Demoted** to one signal in multi-party attestation quorum (M-of-N independent verifiers) | [tee-fail-chief §5.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [tee-fail-lain §1.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-lain.md) |
| FHE layer role | Future enhancement (5 surgical use cases) | **PRIMARY security layer** for RESTRICTED data on Tier B/C hardware | [tee-fail-senku §3.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-qa §3.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md) |
| FHE build phase | Phase 14 (deferred, post-MVP) | **Phase 6** (accelerated, pre-milestone) | [tee-fail-senku §7.1](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-chief §8.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md) |
| DePIN hardware trust | Binary (TEE or sandbox) | **Three-tier**: Tier A (controlled), Tier B (audited DePIN), Tier C (unaudited DePIN) | [tee-fail-senku §5.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-argus §6](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-argus.md) |
| Secret provisioning | Attestation-based delivery to TEE | **Secret Proxy pattern** — secrets stay on Tier A, DePIN agents get FHE-encrypted data or proxied results | [tee-fail-senku §5.4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-lain §4](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-lain.md) |
| Build estimate | 400-600h (14-phase plan) | **516-816h** (15-phase plan, +116-216h for TEE.fail mitigations) | [tee-fail-senku §7.2](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-senku.md), [tee-fail-chief §8.3](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-chief.md), [tee-fail-qa §6](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md) |
| MAST failure modes | 14 failure modes (FM-1 through FM-14) | **18 failure modes** — FM-15 (attestation forgery), FM-16 (bus interposition), FM-17 (deterministic encryption pattern analysis), FM-18 (cascading trust failure) | [tee-fail-qa §1](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/tee-fail-impact-analysis-qa.md) |
