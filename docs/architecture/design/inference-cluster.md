# Shared Inference Cluster Design

**Status:** DRAFT
**Issue:** alternatefutures/admin#132
**Epic:** #129 (Rust Swarm Runtime — 1000-Agent Phase)
**RFC ground truth:** `docs/architecture/reviews/1000-agent-amendment.md` (PR #128)
**Author:** Atlas
**Date:** 2026-04-16
**Coordinates with:** argus (#133) — RESTRICTED POD routing + KV namespace keys; chief-architect (#136) — runtime→cluster proto contract

---

## 1. Purpose and Scope

This document designs the **shared inference cluster** that replaces the per-agent `execFile('claude', ['-p', '--agent'])` subprocess model. All 1,000+ concurrent agents route through this cluster instead of spawning their own model processes.

The cluster is the single largest new workstream in the 1000-agent amendment (+75–100 agent-hours, NEW phase per RFC §3). It must support:

- **1,000+ concurrent in-flight inference requests** at steady state
- **Three model tiers:** Kimi K2.5 (primary orchestration), Claude API (high-stakes only), Qwen2.5-0.5B (classification/routing)
- **Tenant isolation** via KV cache namespace scoping, enforced at the cluster level
- **Hardware tier routing:** OPEN POOL (Tier C, Akash) for PUBLIC/INTERNAL/SENSITIVE; RESTRICTED POD (Tier A) for RESTRICTED tasks

### Out of scope for this design phase

- GPU hardware procurement and provisioning (see OQ-3, §11)
- Full implementation of Ray Serve deployments (skeleton configs only)
- mistral.rs edge deployment (covered in Phase 0 scaffolding; separate path)
- Frontend observability dashboards

---

## 2. Ray Serve Deployment Topology

### 2.1 Overview

```
Rust Runtime (tonic gRPC client)
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Ray Serve Ingress (HTTP/2 gRPC gateway)                        │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  InferenceRouter deployment                              │  │
│  │  - Runs ModelRoleRouter (see §6)                         │  │
│  │  - Emits cost attribution events (see §9)                │  │
│  │  - Routes RESTRICTED tasks to argus-managed Tier A pods  │  │
│  └──────────────┬───────────────┬────────────────┬──────────┘  │
│                 │               │                │              │
│                 ▼               ▼                ▼              │
│  ┌──────────────────┐ ┌──────────────────┐ ┌────────────────┐  │
│  │ KimiK25Deployment│ │ QuenClassifier   │ │ClaudeProxy     │  │
│  │ (vLLM engine)    │ │ (SGLang engine)  │ │Deployment      │  │
│  │                  │ │                  │ │(Claude API)    │  │
│  │ PRIMARY model    │ │ ROUTING TIER     │ │HIGH-STAKES     │  │
│  │ orchestration +  │ │ task classification│ only, budget-  │  │
│  │ complex tasks    │ │ + model selection│ │gated           │  │
│  └──────────────────┘ └──────────────────┘ └────────────────┘  │
│                                                                 │
│  [RESTRICTED POD — Tier A hardware, separate Ray cluster node]  │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  RestrictedKimiK25Deployment (isolated, no shared KV)    │  │
│  │  Managed by argus (#133) — interface defined in §5, §6   │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 InferenceRouter Deployment

The router is the single entry point. It performs:
1. Sensitivity classification (via QuenClassifierDeployment or deterministic DSE tag from Rust runtime)
2. Model role routing (§6)
3. KV cache namespace key injection (§5)
4. Cost attribution event emission (§9)
5. RESTRICTED task forwarding to argus (§6.3)

```python
# Skeleton — not a full implementation
@serve.deployment(
    num_replicas=4,
    ray_actor_options={"num_cpus": 2},
    autoscaling_config={
        "min_replicas": 2,
        "max_replicas": 16,
        "target_ongoing_requests": 50,  # tuned for fast dispatch
        "upscale_delay_s": 5,
        "downscale_delay_s": 30,
    },
)
class InferenceRouter:
    def __init__(self):
        self.kimi = KimiK25Deployment.get_handle()
        self.quen_classifier = QuenClassifierDeployment.get_handle()
        self.claude_proxy = ClaudeProxyDeployment.get_handle()
        self.role_router = ModelRoleRouter()
        self.metrics = ClusterMetrics()

    async def infer(self, request: InferenceRequest) -> InferenceResponse:
        # 1. Classify if sensitivity not already set by DSE
        if request.sensitivity_level == SensitivityLevel.UNSET:
            classification = await self.quen_classifier.classify.remote(request)
            request = request.with_classification(classification)

        # 2. Route RESTRICTED tasks out-of-band to argus
        if request.sensitivity_level == SensitivityLevel.RESTRICTED:
            return await self._forward_to_restricted_pod(request)

        # 3. Normal path: model role routing
        target_model = self.role_router.route(request)
        ns_key = KVNamespaceKey.for_request(request)  # §5
        request = request.with_kv_namespace(ns_key)

        # 4. Dispatch
        response = await self._dispatch(target_model, request)
        self.metrics.record(request, response)
        return response
```

### 2.3 KimiK25Deployment

```python
@serve.deployment(
    name="kimi-k25",
    num_replicas=2,
    ray_actor_options={"num_gpus": 4},  # tensor_parallel_size=4 per replica
    autoscaling_config={
        "min_replicas": 1,
        "max_replicas": 8,
        "target_ongoing_requests": 128,  # vLLM continuous batching handles queuing
        "upscale_delay_s": 10,
        "downscale_delay_s": 60,
    },
)
class KimiK25Deployment:
    def __init__(self):
        from vllm import AsyncLLMEngine, AsyncEngineArgs
        args = AsyncEngineArgs(
            model="moonshot-ai/kimi-k2.5",  # local path once weights acquired
            tensor_parallel_size=4,
            max_num_seqs=256,           # per-replica; 2 replicas → 512 concurrent slots
            max_model_len=131072,       # 128K context per Kimi K2.5 spec
            enable_chunked_prefill=True,
            max_num_batched_tokens=8192,
            block_size=16,              # PagedAttention block size (tokens per block)
            gpu_memory_utilization=0.90,
            quantization=None,          # BF16 default; consider AWQ if VRAM constrained
            enable_prefix_caching=True, # shared prefix cache (§5)
        )
        self.engine = AsyncLLMEngine.from_engine_args(args)
```

### 2.4 QuenClassifierDeployment

```python
@serve.deployment(
    name="quen-classifier",
    num_replicas=4,
    ray_actor_options={"num_gpus": 0.5},  # fractional GPU; small model
    autoscaling_config={
        "min_replicas": 2,
        "max_replicas": 20,
        "target_ongoing_requests": 200,  # fast, low-latency classification
        "upscale_delay_s": 3,
        "downscale_delay_s": 20,
    },
)
class QuenClassifierDeployment:
    def __init__(self):
        import sglang as sgl
        # SGLang server for Qwen2.5-0.5B — see §3 for engine choice rationale
        self.runtime = sgl.Runtime(
            model_path="Qwen/Qwen2.5-0.5B-Instruct",
            tp_size=1,
            context_length=4096,
            max_running_requests=256,
        )
```

### 2.5 ClaudeProxyDeployment

```python
@serve.deployment(
    name="claude-proxy",
    num_replicas=2,
    ray_actor_options={"num_cpus": 1},
    autoscaling_config={
        "min_replicas": 1,
        "max_replicas": 4,
        "target_ongoing_requests": 10,  # intentionally low — Claude is expensive
    },
)
class ClaudeProxyDeployment:
    def __init__(self):
        self.budget_gate = ClaudeBudgetGate(
            global_hourly_token_budget=500_000,
            per_tenant_hourly_budget=50_000,
        )
        self.circuit_breaker = CircuitBreaker(
            failure_threshold=5,
            recovery_timeout_s=60,
        )
```

---

## 3. vLLM Engine Config — Kimi K2.5 for 1K Concurrent

### 3.1 PagedAttention

vLLM's PagedAttention manages KV cache as fixed-size pages, eliminating memory fragmentation that would cause OOM under 1K concurrent sequences. Key parameters:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `block_size` | 16 tokens | Standard; larger (32) reduces overhead but increases waste on short sequences |
| `gpu_memory_utilization` | 0.90 | Leaves 10% headroom for activations and CUDA overhead |
| `max_model_len` | 131072 | Kimi K2.5's 128K context window; cap at actual max to avoid over-allocation |
| `swap_space` | 8 GiB | CPU offload for KV blocks of preempted sequences |

Under 1K concurrent agents, most will be in queue rather than actively generating. PagedAttention's non-contiguous KV storage allows held sequences to yield GPU blocks to active generators without copying, enabling high effective concurrency.

### 3.2 Continuous Batching

vLLM's continuous (iteration-level) batching allows new requests to join an in-flight batch at each decoder step, rather than waiting for a batch to complete. Configuration:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `max_num_seqs` | 256 per replica | With 4 replicas → 1,024 total concurrent sequences at target load |
| `max_num_batched_tokens` | 8192 | Prefill + decode budget per iteration; prevents long-context prefills from stalling decode |
| `scheduler_delay_factor` | 1.5 | FCFS with mild delay to allow micro-batch formation |

**Replica scaling for 1K:** 4 replicas × 256 max_num_seqs = 1,024 total slots. Autoscaler caps at 8 replicas (2,048 slots) for burst. `target_ongoing_requests=128` per replica triggers scale-up before queues build.

### 3.3 Chunked Prefill

Long system prompts (agent context, memory injection) can block the decode step for hundreds of milliseconds. Chunked prefill splits long prompts across iterations:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `enable_chunked_prefill` | `True` | Required at 1K scale |
| `max_num_batched_tokens` | 8192 | Each iteration processes at most 8K tokens across prefill + decode |

With agents carrying 4–32K token contexts (memory injection), chunking prevents any single prefill from monopolizing the batch slot.

### 3.4 Tensor Parallelism

Kimi K2.5 is a large MoE model. Assuming H100 80GB GPUs:

```
Estimated model size (BF16): ~140GB active parameters (MoE activates subset per token)
Tensor parallel across 4× H100: ~35GB per GPU — fits with headroom for KV cache
```

`tensor_parallel_size=4` per replica. Two replicas = 8 H100s minimum at baseline. Scale to 8 replicas = 32 H100s at peak. Actual sizing depends on weight acquisition (see OQ-2, §11).

---

## 4. SGLang for Qwen2.5-0.5B (Classification Tier)

### 4.1 SGLang vs vLLM for This Role

The classification tier routes each request to the correct model and extracts the sensitivity label (if not already set by the Rust DSE). This is a **structured output** task: the model always returns a fixed JSON schema with `{task_type, model_hint, estimated_complexity, sensitivity_level}`.

| Criterion | SGLang | vLLM |
|-----------|--------|------|
| Constrained decoding (JSON schema) | Native (grammar-guided) | Plugin required |
| RadixAttention prefix cache reuse | Built-in | `enable_prefix_caching=True` |
| Throughput on small model | Comparable | Comparable |
| Latency for short outputs | Slightly faster (constrained decoding prunes beam) | Slightly slower |
| Operational complexity | One more framework in stack | Unified with KimiK25 |

**Decision: SGLang preferred** for QuenClassifierDeployment. The classification prompt has a fixed system prompt (high cache hit rate via RadixAttention) and always produces a short, schema-constrained JSON response. SGLang's native grammar-guided decoding eliminates JSON parsing failures without post-processing.

Note: This introduces SGLang as an additional framework alongside vLLM. The operational cost is accepted because the classification tier's performance directly determines end-to-end routing latency for all 1K concurrent agents.

### 4.2 Classification Schema

```json
{
  "task_type": "orchestration|tool_use|classification|generation|reasoning|retrieval",
  "model_hint": "kimi_k25|claude|local_quen",
  "estimated_complexity": "low|medium|high",
  "sensitivity_level": "PUBLIC|INTERNAL|SENSITIVE|RESTRICTED",
  "confidence": 0.0
}
```

The `model_hint` is advisory — the `ModelRoleRouter` (§6) may override it based on budget state and routing policy.

### 4.3 SGLang Deployment Config

```python
# SGLang Runtime for Qwen2.5-0.5B-Instruct
sgl.Runtime(
    model_path="Qwen/Qwen2.5-0.5B-Instruct",
    tp_size=1,
    context_length=4096,        # classification prompts are short
    max_running_requests=256,
    enable_radix_cache=True,    # prefix cache for system prompt reuse
    constrained_json_whitespace_pattern=r"[\n ]*",  # allow compact JSON
)
```

---

## 5. KV Cache Design

### 5.1 Shared Prefix Cache

vLLM's `enable_prefix_caching=True` enables hash-based prefix matching across requests. Requests from different agents with the same system prompt prefix share cached KV blocks, reducing prefill cost.

Common shared prefixes in the AF swarm:
- Agent persona system prompts (same per agent type)
- Tool manifests (same per task category)
- Memory injection headers (variable, lower cache hit rate)

### 5.2 Tenant Namespace Scoping

**CRITICAL ISOLATION REQUIREMENT:** KV cache blocks must not leak between tenants even when system prompts collide by hash. This is RFC FM-20 (cross-tenant KV cache poisoning).

Namespace key schema (coordinated with argus):

```
Format: {tenant_id}:{sensitivity_level}:{agent_type}:{prefix_hash}

Examples:
  acme_corp:INTERNAL:orchestrator:sha256_abc123
  beta_tenant:SENSITIVE:code-agent:sha256_def456
  RESTRICTED:tenant_xyz:*  →  isolated pod only (never in OPEN POOL)
```

Implementation approach:
- Prefix all cache keys with `{tenant_id}:` — vLLM does not natively namespace by tenant
- **Required:** custom `CacheNamespaceMiddleware` layer injected between router and vLLM that prepends the namespace to all cache lookups
- Hash-collision attack: two tenants with identical system prompts get different namespace keys, so they always miss each other's cache. This is intentional (some cache efficiency sacrificed for isolation)

```rust
// Rust-side namespace key construction (in af-inference-client)
pub struct KVNamespaceKey {
    pub tenant_id: TenantId,
    pub sensitivity_level: SensitivityLevel,
    pub agent_type: AgentType,
    pub prefix_hash: [u8; 32], // SHA-256 of system prompt prefix
}

impl KVNamespaceKey {
    pub fn for_request(req: &InferenceRequest) -> Self {
        let prefix_hash = sha256(req.system_prompt.as_bytes());
        Self {
            tenant_id: req.tenant_id.clone(),
            sensitivity_level: req.sensitivity_level,
            agent_type: req.agent_type,
            prefix_hash,
        }
    }

    pub fn to_cache_prefix(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.tenant_id,
            self.sensitivity_level.as_str(),
            self.agent_type.as_str(),
            hex::encode(self.prefix_hash)
        )
    }
}
```

### 5.3 RESTRICTED POD KV Cache

RESTRICTED tasks are routed to a completely separate Ray cluster node (Tier A hardware, managed by argus). The RESTRICTED POD:
- Has its own isolated vLLM instance
- Shares KV cache **only within a single tenant's RESTRICTED namespace** — no cross-tenant sharing even within Tier A
- Uses namespace key: `RESTRICTED:{tenant_id}:{agent_type}:{prefix_hash}`
- Is mTLS-separated from the OPEN POOL network segment

**Interface contract with argus (#133):** The `InferenceRouter` forwards RESTRICTED requests over an internal gRPC channel to the `RestrictedPodGateway` (owned by argus). The `InferenceRouter` does NOT inspect or log the content of RESTRICTED requests beyond the routing metadata.

```proto
// Defined in §7 (inference.proto), called by InferenceRouter for RESTRICTED tasks
rpc ForwardToRestrictedPod(RestrictedInferenceRequest) returns (InferenceResponse);
```

**Open question for argus:** Should the namespace key schema above be the canonical form, or does argus need additional fields (e.g., clearance_level, compartment_id) for multi-compartment RESTRICTED isolation?

---

## 6. Model Role Router

### 6.1 Routing Rules (Primary)

The `ModelRoleRouter` is rule-based in Phase 1. A learned classifier is deferred (see §6.4).

```
INPUT: sensitivity_level, task_type, estimated_complexity, tenant_budget_remaining

ROUTING LOGIC (priority order):

1. RESTRICTED → RestrictedPod (forward to argus, stop here)

2. task_type == "classification" OR estimated_complexity == "low"
   → QuenClassifier (self-loop, classification already done, re-route on result)

3. sensitivity_level == SENSITIVE AND complexity == "high"
   AND tenant_budget_remaining > 0 AND global_claude_budget_remaining > 0
   → ClaudeProxy

4. DEFAULT (orchestration, tool_use, reasoning, generation, medium/high complexity)
   → KimiK25

5. FALLBACK (KimiK25 circuit breaker open or all slots full)
   → ClaudeProxy if budget permits, else queue with backpressure
```

### 6.2 Budget Signal Integration

The router consults `ClaudeBudgetGate` before any Claude route:

```rust
pub enum RouteDecision {
    KimiK25,
    ClaudeProxy,
    QuenClassifier,
    RestrictedPod,
    Queued { reason: QueueReason },
}

pub struct ModelRoleRouter {
    kimi_health: Arc<AtomicBool>,
    claude_budget: Arc<ClaudeBudgetGate>,
}

impl ModelRoleRouter {
    pub fn route(&self, req: &InferenceRequest) -> RouteDecision {
        match req.sensitivity_level {
            SensitivityLevel::Restricted => RouteDecision::RestrictedPod,
            _ => self.route_open_pool(req),
        }
    }

    fn route_open_pool(&self, req: &InferenceRequest) -> RouteDecision {
        if req.task_type == TaskType::Classification {
            return RouteDecision::QuenClassifier;
        }
        if req.requires_claude() && self.claude_budget.has_capacity(req.tenant_id) {
            return RouteDecision::ClaudeProxy;
        }
        if self.kimi_health.load(Ordering::Relaxed) {
            return RouteDecision::KimiK25;
        }
        // Kimi unavailable, Claude budgeted
        if self.claude_budget.has_capacity(req.tenant_id) {
            return RouteDecision::ClaudeProxy;
        }
        RouteDecision::Queued { reason: QueueReason::AllModelsUnavailable }
    }
}
```

### 6.3 RESTRICTED Task Forwarding Interface (argus coordination)

The router must signal argus when a RESTRICTED task arrives. The proposed interface:

1. The `InferenceRequest` proto carries `sensitivity_level = RESTRICTED` and `tenant_id`
2. The router calls `ForwardToRestrictedPod` RPC (see §5.3, §7)
3. The response is an opaque `InferenceResponse` — the router does not inspect content
4. The router records only the routing event for cost attribution (token counts but not content)

This boundary must be reviewed with argus for correctness.

### 6.4 Future: Learned Router

A Qwen2.5-0.5B fine-tuned classifier could replace the rule-based `requires_claude()` heuristic. Deferred to post-Phase-2. No design work needed now.

---

## 7. Claude API Proxy — Rate-Limit Gating and Circuit Breaker

### 7.1 Budget Pool Design

The cluster maintains a shared token budget for Claude API calls. Budget state is stored in Redis (or Ray's distributed state store) to survive single-deployment restarts.

```
Global hourly budget: 500K tokens (configurable)
Per-tenant hourly budget: 50K tokens (configurable)
Per-request hard cap: 8K tokens (prevents single-agent monopoly)
```

Budget is enforced at two levels:
1. **Pre-flight check** in `ModelRoleRouter` (§6.2) — routes away from Claude if budget exhausted
2. **Hard gate** in `ClaudeProxyDeployment.__call__` — returns `BUDGET_EXHAUSTED` error if pre-flight check was stale

### 7.2 ClaudeBudgetGate

```rust
pub struct ClaudeBudgetGate {
    global_tokens_used: Arc<AtomicU64>,
    global_hourly_limit: u64,
    tenant_budgets: Arc<DashMap<TenantId, AtomicU64>>,
    per_tenant_hourly_limit: u64,
    window_reset_at: Arc<Mutex<Instant>>,
}

impl ClaudeBudgetGate {
    pub fn has_capacity(&self, tenant: &TenantId) -> bool {
        self.check_global() && self.check_tenant(tenant)
    }

    pub fn record_usage(&self, tenant: &TenantId, tokens: u64) {
        self.global_tokens_used.fetch_add(tokens, Ordering::Relaxed);
        self.tenant_budgets
            .entry(tenant.clone())
            .or_default()
            .fetch_add(tokens, Ordering::Relaxed);
    }
}
```

### 7.3 Circuit Breaker

The circuit breaker protects against Claude API degradation propagating into the cluster:

```
STATES: CLOSED (normal) → OPEN (failing) → HALF_OPEN (testing recovery)

Thresholds:
  failure_threshold: 5 consecutive errors in 30s window → OPEN
  recovery_timeout:  60 seconds in OPEN before attempting HALF_OPEN
  success_threshold: 3 successes in HALF_OPEN → CLOSED

On OPEN:
  - Route to KimiK25 if available
  - Reject with ClaudeUnavailable if KimiK25 also unavailable
  - Emit circuit_breaker_open metric (see §9)
```

### 7.4 Rate Limit Header Parsing

The proxy parses Claude's `x-ratelimit-remaining-requests` and `x-ratelimit-reset-requests` headers and feeds them back into `ClaudeBudgetGate` to maintain accurate remaining capacity.

---

## 8. Request/Response Proto (gRPC)

### 8.1 Proto Location

```
crates/af-inference-client/
├── proto/
│   ├── inference.proto          ← primary service contract
│   └── common.proto             ← shared enums/types
├── src/
│   ├── lib.rs
│   └── client.rs                ← tonic-generated stubs (skeleton only)
└── build.rs                     ← prost build script
```

See `crates/af-inference-client/proto/inference.proto` (committed alongside this doc).

### 8.2 Proto Design Summary

The proto defines:
- `InferenceService` with `Infer` (unary) and `StreamInfer` (server-streaming) RPCs
- `InferenceRequest` carrying agent metadata, prompt content, sensitivity level, routing hints
- `InferenceResponse` with model attribution, usage counters, latency
- `ClassifyRequest` / `ClassifyResponse` for direct classification (bypasses routing)
- `RestrictedInferenceRequest` for the argus handoff (stripped of content, routing-only header)

**Coordination with chief-architect (#136):** The proto defined here is the cluster-facing surface. The Rust runtime (Dapr actor side) calls this cluster via tonic. The proto contract between Dapr actor and the `af-inference-client` crate should be reviewed by chief-architect to ensure the actor messaging model is compatible with the gRPC call semantics (notably: does the actor await the response, or does it fire-and-forget via JetStream?).

**Open question for chief-architect:** Should `StreamInfer` use server-side streaming or bidirectional streaming? Bidirectional would allow mid-generation cancellation from the Rust runtime (e.g., if the MAST failure detector aborts the task), but adds implementation complexity.

---

## 9. Cost Attribution Metrics

### 9.1 Prometheus Label Design

All inference metrics use this label set:

```
Labels:
  agent_id       - individual agent instance ID (high cardinality, use sparingly)
  agent_type     - agent role (orchestrator, code-agent, marketing-agent, etc.)
  tenant_id      - billing tenant
  model          - "kimi_k25" | "claude_claude-3-5-sonnet" | "quen_0.5b"
  task_type      - "orchestration" | "tool_use" | "classification" | "generation" | "reasoning" | "retrieval"
  sensitivity    - "PUBLIC" | "INTERNAL" | "SENSITIVE" | "RESTRICTED"
  routed_to_pod  - "open_pool" | "restricted_pod"
  status         - "success" | "error" | "budget_exhausted" | "circuit_open" | "timeout"
```

**Cardinality warning:** `agent_id` × `tenant_id` × `model` can be very high (1K agents × N tenants). Use `agent_type` aggregations for dashboards; reserve `agent_id` label for per-agent debugging only (store in trace spans, not metrics).

### 9.2 Metric Definitions

```prometheus
# Counters
inference_requests_total{agent_type, tenant_id, model, task_type, sensitivity, status}
inference_tokens_in_total{agent_type, tenant_id, model}
inference_tokens_out_total{agent_type, tenant_id, model}
inference_cost_usd_total{agent_type, tenant_id, model}  # computed from token counts × model rate

# Histograms
inference_latency_seconds{agent_type, model, task_type}  # buckets: .05 .1 .25 .5 1 2.5 5 10
inference_queue_wait_seconds{model}

# Gauges
inference_budget_remaining_tokens{tenant_id, model}      # for Claude proxy
inference_cluster_active_sequences{model, replica_id}    # vLLM concurrent sequences
circuit_breaker_state{model}                             # 0=CLOSED, 1=HALF_OPEN, 2=OPEN
```

### 9.3 Cost Computation

Cost is attributed at response time using the token counts from the model response. Claude pricing is injected as a config value (not hardcoded):

```rust
pub struct CostAttributor {
    model_rates: HashMap<ModelId, TokenRate>,  // from cluster config, hot-reloadable
}

impl CostAttributor {
    pub fn attribute(&self, resp: &InferenceResponse) -> CostEvent {
        let rate = self.model_rates.get(&resp.model_id).unwrap_or(&TokenRate::ZERO);
        let cost_usd = (resp.usage.prompt_tokens as f64 * rate.input_per_token)
            + (resp.usage.completion_tokens as f64 * rate.output_per_token);
        CostEvent {
            tenant_id: resp.tenant_id.clone(),
            agent_id: resp.agent_id.clone(),
            model_id: resp.model_id.clone(),
            cost_usd,
            tokens_in: resp.usage.prompt_tokens,
            tokens_out: resp.usage.completion_tokens,
        }
    }
}
```

---

## 10. Ingress: Rust Runtime → Ray Serve

### 10.1 Protocol: gRPC over HTTP/2

The Rust runtime connects to the Ray Serve gRPC gateway via `tonic`. Rationale:
- tonic is already in the v3.0 tech stack (used for A2A protocol)
- HTTP/2 multiplexing handles 1K concurrent requests on a small number of TCP connections
- Proto-defined contract enforces schema evolution discipline
- Bidirectional streaming supports token-by-token streaming responses

**NOT** using NATS as the ingress path for inference requests. NATS (L3 messaging) remains for agent-to-agent coordination; inference is a request-response loop that benefits from direct gRPC connection management.

### 10.2 Connection Pool

```rust
pub struct InferenceClusterClient {
    inner: InferenceServiceClient<tonic::transport::Channel>,
    // Tonic's Channel handles connection pooling, load balancing, and retry internally
}

impl InferenceClusterClient {
    pub async fn connect(endpoint: &str) -> Result<Self, ClientError> {
        let channel = tonic::transport::Endpoint::new(endpoint)?
            .connect_lazy();  // lazy connect; reconnects automatically
        Ok(Self { inner: InferenceServiceClient::new(channel) })
    }
}
```

The `Channel` is shared across all Dapr actor instances in the process. Connection count is bounded by the gRPC connection pool (default: 1 per endpoint; configurable).

### 10.3 Backpressure

When all vLLM slots are full:
1. Ray Serve returns `RESOURCE_EXHAUSTED` (gRPC status 8)
2. `af-inference-client` surfaces this as `InferenceError::ClusterFull`
3. Dapr actor schedules retry with exponential backoff (max 3 attempts, 50ms base)
4. After 3 attempts: task enters `Failed` state, MAST failure detector is notified (FM-19)

### 10.4 Batching Strategy

Requests are NOT batched at the Rust runtime layer. vLLM's continuous batching already groups requests optimally at the model level. Batching at the Rust layer would add latency without benefit.

**Exception:** Classification requests from the routing tier could be batched (Qwen2.5-0.5B is fast and cheap). Future optimization; not in Phase 1.

---

## 11. Failure Modes

Mapped to the MAST failure taxonomy (v3.0 §Failure_Taxonomy). This section adds FM-19 and FM-20 (from RFC §7) and defines cluster-specific recovery behavior.

### FM-19: Inference Cluster Partition (NEW — RFC §7)

**Trigger:** All Ray Serve replicas unreachable, or Ray head node failure.

**Detection:** tonic connection timeout after 5s; 3 consecutive health check failures (probe interval: 10s).

**Impact:** All in-flight inference requests fail. Dapr actors blocked waiting for inference responses will timeout.

**Recovery:**
1. Circuit breaker opens immediately on detection
2. Pending agent tasks queued in JetStream (durable, not lost)
3. Partial fallback: if mistral.rs edge path is available (Tier C, no GPU), simple tasks route there
4. Claude API proxy remains available (it's HTTP, not Ray-dependent) — budget-gated escalation
5. On cluster recovery: circuit breaker closes after 3 successful probes; JetStream queue drains

**MAST classification:** `ClusterPartition` → severity HIGH, affects all agents.

### FM-20: Cross-Tenant KV Cache Poisoning (NEW — RFC §7)

**Trigger:** Bug in `CacheNamespaceMiddleware` causes tenant A's cached KV blocks to be served to tenant B.

**Detection:** Anomaly detection on response content (statistical divergence from expected output distribution). Manual audit trigger if cross-tenant data is reported.

**Prevention (primary):** Namespace key includes `{tenant_id}` as first component — any cache hit requires matching tenant prefix. Hash collision probability: negligible (SHA-256).

**Recovery:**
1. Immediate: flush KV cache for affected tenants
2. Alert security team; treat as SENSITIVE data incident
3. Post-incident: audit `CacheNamespaceMiddleware` hash partitioning logic

**Note for argus:** RESTRICTED POD is immune to FM-20 by design (completely separate cluster node with no shared KV state).

### FM-21: Single Model OOM (Kimi K2.5 Replica)

**Trigger:** A Kimi K2.5 replica exceeds GPU memory. Causes: sequence longer than `max_model_len`, KV cache fully exhausted by active sequences.

**Detection:** vLLM OOM exception caught in `KimiK25Deployment.generate()`; Ray marks replica unhealthy.

**Recovery:**
1. Ray Serve routes new requests to remaining healthy replicas
2. OOM'd replica restarts (Ray actor restart policy: max 3 restarts in 10 minutes)
3. In-flight requests on failed replica: vLLM acknowledges preemption via RESOURCE_EXHAUSTED; Rust runtime retries on new replica (backpressure path in §10.3)
4. If all replicas OOM simultaneously: FM-19 path activates

**Mitigation:** `gpu_memory_utilization=0.90` leaves 10% headroom. `max_num_seqs=256` caps concurrent sequences before OOM threshold. Monitor `inference_cluster_active_sequences` gauge.

### FM-22: K2.5 Inference Timeout

**Trigger:** A single Kimi K2.5 request exceeds the timeout threshold (default: 120s for complex orchestration tasks).

**Detection:** tonic request deadline exceeded (gRPC status DEADLINE_EXCEEDED).

**Recovery:**
1. `af-inference-client` returns `InferenceError::Timeout`
2. Dapr actor can retry (if task is idempotent) or fail the task
3. MAST records timeout; if timeout rate > 5% in 60s window, suspect cluster degradation → FM-19 path

**Timeout configuration:**
```
Classification requests: 5s deadline
Standard generation: 60s deadline
Complex orchestration: 120s deadline
```

---

## 12. Open Questions

### OQ-A: Kimi K2.5 License (Blocks Deployment) — maps to RFC OQ-2

Kimi K2.5 weights are assumed self-hosted in this design. Commercial use requires legal review of the Moonshot AI license terms. **This is a hard blocker for the inference cluster going to production.** Design work can proceed, but GPU procurement and model download must not start until legal sign-off.

**Action required:** Legal review of Kimi K2.5 license. See RFC §6.

### OQ-B: GPU Hardware for OPEN POOL — maps to RFC OQ-3

This design assumes H100 80GB GPUs. The actual cluster hardware (hyperscaler vs dedicated vs DePIN) is unresolved. H100 availability affects:
- `tensor_parallel_size` setting
- `max_num_seqs` per replica
- Total replica count for 1K concurrent target

**Action required:** Infrastructure owner decision on GPU provisioning. Design remains valid across GPU types; parameters above may need tuning.

### OQ-C: Runtime→Cluster Call Semantics (for chief-architect #136)

The `af-inference-client` exposes a synchronous tonic `Infer` RPC. It is assumed the Dapr actor awaits this call. If Dapr actors use an async message-passing model (JetStream reply-to pattern), the `StreamInfer` RPC may be more appropriate.

**Action required:** chief-architect to confirm whether actors block on inference or use async reply-to. Proto remains valid in both cases; implementation of the client in `crates/af-inference-client/src/client.rs` will differ.

### OQ-D: RESTRICTED POD Routing Interface (for argus #133)

The namespace key schema in §5.2 and the `ForwardToRestrictedPod` RPC in §5.3 are proposed. Argus should confirm:
1. Is the namespace key schema (`RESTRICTED:{tenant_id}:{agent_type}:{prefix_hash}`) sufficient for RESTRICTED compartmentalization?
2. Does argus need additional fields in `RestrictedInferenceRequest` (clearance_level, compartment_id)?
3. Who owns the `ForwardToRestrictedPod` RPC server implementation — argus or this cluster?

### OQ-E: SGLang Operational Complexity

Introducing SGLang alongside vLLM adds a second inference framework to operate. If this is unacceptable, vLLM's `guided_decoding` (outlines backend) is an alternative for the classification tier, at some performance cost. Recommend SGLang, but flagging for ops team review.

### OQ-F: Kimi K2.5 vs K2.6 — maps to RFC OQ-6

If Kimi K2.6 ships before the cluster reaches production, the vLLM config above may need revision. Architecture is model-agnostic at the Ray Serve level; `KimiK25Deployment` would be renamed/parameterized. No design change required — flagging for awareness.

---

## 13. Coordination Summary

| Party | Coordination item | Status |
|-------|-------------------|--------|
| argus (#133) | RESTRICTED POD routing interface (§5.3, §6.3); KV namespace key schema (§5.2) | ⏳ Pending argus review |
| chief-architect (#136) | Runtime→cluster proto contract (§8); actor call semantics (OQ-C) | ⏳ Pending #136 design |
| Legal | Kimi K2.5 commercial license (OQ-A, RFC OQ-2) | ⏳ Blocking for production |
| Infra/Ops | GPU hardware for OPEN POOL (OQ-B, RFC OQ-3) | ⏳ Unresolved |

---

*This document is the design phase artifact for issue #132. Full implementation is not expected in this phase.*


---

## 14. Integration Contract Interface Spec (chief-architect #141 Response)

This section was added by the integration-addendum PR (branched from `feat/issue-132-inference-cluster`). It responds explicitly to the open items in `integration-contract.md` (PR #141) that were deferred until PR #132 opened. It is the authoritative resolution of those items from Atlas.

**Items closed by this section:**
- **CL-004** — KV cache namespace key format (Argus/Atlas coordination)
- **OQ-C** — Runtime→cluster actor call semantics (sync vs async)
- **§8 placeholder** — "Atlas (#132) amendment pending — will add inference cluster API shapes to §3.2"

---

### 14.1 CL-004 Response: KV Cache Namespace Key Amendment

**Integration contract interim spec (CL-004):**
```
{tenant_id}:{dse_tier}:{team_id}     where dse_tier ∈ {pub, int, sen, res}
```

**This design amends CL-004** by extending to a 4-component key:

```
{tenant_id}:{sensitivity_level}:{agent_type}:{prefix_hash}
```

| Component | Type | Example values | Rationale |
|-----------|------|----------------|-----------|
| `tenant_id` | UUID or slug | `acme_corp`, `3fa85f64-…` | Tenant isolation root — mandatory first component per CL-004 ordering constraint |
| `sensitivity_level` | 3-char enum | `pub`, `int`, `sen`, `res` | Maps 1:1 to CL-004's `dse_tier` abbreviations (same value set, same ordering position) |
| `agent_type` | slug | `orchestrator`, `worker`, `classifier` | Sub-tenant partition: different agent classes within the same team need isolated KV namespaces for cache coherence; replaces the coarser `team_id` |
| `prefix_hash` | 8-char hex | `a1b2c3d4` | SHA-256 prefix of system prompt — enables prefix-level cache invalidation without full namespace flush; not present in the interim 3-component spec |

**Ordering invariant preserved:** `tenant_id` remains the first component. Prefix-based namespace invalidation (`KEYS {tenant_id}:*`) across all tiers for a given tenant works correctly, satisfying the binding constraint in CL-004.

**Compatibility with Argus #138 §4.2:** The `{tenant_id}:{sensitivity_level}` 2-component prefix remains a valid query. The `{team_id}` component in the CL-004 interim spec is superseded by `{agent_type}:{prefix_hash}` (more granular; backward-compatible for prefix operations).

**RESTRICTED namespace exception:** RESTRICTED-tier keys use a different leading literal:
```
RESTRICTED:{tenant_id}:{agent_type}:{prefix_hash}
```
This ensures RESTRICTED keys are structurally distinct and cannot be matched by wildcard queries against OPEN POOL namespaces.

**Required action for Argus (#138):** Update §4.2 of `security-multi-tenant.md`. The canonical proto field is `KvCacheNamespaceKey` in `crates/af-inference-client/proto/inference.proto` (PR #139). If Argus needs additional compartment fields (e.g., `clearance_level`), propose an amendment here.

---

### 14.2 OQ-C Resolution: Actor Call Semantics — Server-Streaming Recommended

**Open question (PR #139 §12 OQ-C, integration-contract pending):**
Should OrchestratorActor call inference via sync unary `Infer` or async JetStream reply-to?

**Decision: Server-streaming gRPC (`StreamInfer`) is the primary path. Unary `Infer` is retained only for completions under ~512 output tokens.**

Rationale:

1. **Actor semantics fit server-streaming, not bidirectional:** OrchestratorActor sends a task and receives tokens as they arrive. Server-streaming maps cleanly to this — the actor receives a `Streaming<StreamInferResponse>` and emits partial results downstream without blocking. Bidirectional streaming implies the server can push unsolicited data, which does not fit the compute-service model and complicates Ray Serve horizontal scaling.

2. **JetStream reply-to is incorrect for inference:** JetStream provides at-least-once delivery. A replayed inference call on network partition is both incorrect (duplicate output) and expensive (wasted GPU compute). Core NATS fire-and-forget is reserved for agent messages per integration-contract INT-002. Inference is synchronous compute, not event delivery.

3. **First-token latency:** The failure mode FM-22 sets a 120s deadline for complex orchestration. Server-streaming delivers first token immediately; unary would buffer the full response, masking progress and making partial-result recovery impossible.

4. **Practical tonic implementation:** `InferenceClient::stream_infer(request)` returns `Streaming<StreamInferResponse>`. OrchestratorActor awaits each chunk in a tokio select loop and forwards to its output channel. This pattern is idiomatic in the Rust async actor model (ractor).

**Implementation note for Lain (#137):** Use `tonic` streaming client. `Infer` (unary) remains valid for classification calls from `QuenClassifier` where full response is needed before routing — these are always short.

---

### 14.3 §3.2 API Shape Summary (Integration Contract Amendment)

This is the canonical runtime→inference interface that chief-architect (#141) references in `integration-contract.md §3.2`. It supersedes the `proto/af-swarm/runtime_inference.proto` skeleton in PR #141 — this PR's `crates/af-inference-client/proto/inference.proto` is the ground truth.

**gRPC service:** `af.inference.v1.InferenceService`
**Proto source:** `crates/af-inference-client/proto/inference.proto` (PR #139)
**Transport:** tonic over HTTP/2, mTLS; identity asserted via `InferenceIdentityClaim` header (Ed25519 / NKey)

**Primary RPC — streaming:**
```protobuf
rpc StreamInfer(InferRequest) returns (stream StreamInferResponse);

message InferRequest {
  string agent_id    = 1;   // caller identity
  string tenant_id   = 2;   // billing root + KV namespace prefix
  SensitivityLevel sensitivity = 3;   // routes to OPEN POOL or RESTRICTED POD
  InferenceTier tier = 4;   // explicit Tier A override (RESTRICTED tasks)
  repeated Message messages  = 5;
  GenerationConfig config    = 6;
  TraceContext trace_context = 7;   // OTel W3C propagation
  CostBudget budget          = 8;   // per-request cap; exceed → AF-007
}

message StreamInferResponse {
  string chunk         = 1;
  bool   is_final      = 2;
  UsageSummary usage   = 3;   // populated only when is_final = true
  string routed_to     = 4;   // "kimi_k25" | "claude_api" | "restricted_pod" | "classifier"
  string trace_id      = 5;
}
```

**Secondary RPC — unary (short completions, classification):**
```protobuf
rpc Infer(InferRequest) returns (InferResponse);
```

**Error codes (canonical AF codes, no fallback for RESTRICTED):**

| Code | Meaning | Recovery |
|------|---------|----------|
| `AF-006` | RESTRICTED POD unavailable — no OPEN POOL fallback | Reject task; surface to tenant |
| `AF-007` | Per-request budget exhausted | Queue or reject; emit cost-attribution metric |
| `AF-008` | Cluster full (`RESOURCE_EXHAUSTED`) | Exponential backoff; 3 retries max, then fail |

**OTel trace context:** `trace_context` field propagates the W3C `traceparent` header across the runtime→cluster boundary. This is the boundary #7 in the integration-contract cross-cutting OTel table.

---

### 14.4 Multi-Provider Infra Note

Per the platform constraint (AWS + Akash, not Akash-only):

- **RESTRICTED POD (Tier A):** Runs on AWS EKS with dedicated GPU nodes. Tenant data must not leave AWS. The Ray Serve `RestrictedKimiK25Deployment` and `RestrictedPodGateway` service are AWS-only.
- **OPEN POOL (Tier B/C):** Runs on Akash GPU providers. `KimiK25Deployment`, `QuenClassifierDeployment`, and `ClaudeProxyDeployment` are Akash-hosted.
- **Ray Serve ingress:** The gRPC gateway is the only component that spans both; it is deployed on AWS and forwards OPEN POOL requests over a private channel to Akash workers. Akash workers are not directly addressable from the Rust runtime.
- **Argus OQ-9-A3 coordination (RESTRICTED vector store):** RESTRICTED inference operations do not cross to Akash. If the RESTRICTED vector store (Argus #138 §6.3) is on AWS Tier A, the `ForwardToRestrictedPod` RPC path is fully AWS-internal. Argus should confirm the vector store deployment tier matches.

---

### 14.5 Coordination Table Update

| Party | Item | Status after this addendum |
|-------|------|---------------------------|
| Argus (#138) | CL-004: KV namespace key | **Closed by §14.1** — 4-component key confirmed; Argus must update §4.2 |
| Chief-architect (#141) | OQ-C: actor call semantics | **Closed by §14.2** — server-streaming (`StreamInfer`) is primary |
| Chief-architect (#141) | §8 Amendment Log placeholder | **Closed by §14.3** — API shapes defined; §8 row should reference this PR |
| Argus (#138) | OQ-D: RESTRICTED namespace sufficiency | ⏳ Still open — Argus must confirm 4-component key covers compartmentalization needs |
| Chief-architect (#141) | §3.2 proto ground truth | **Closed** — `crates/af-inference-client/proto/inference.proto` supersedes `proto/af-swarm/runtime_inference.proto` |
