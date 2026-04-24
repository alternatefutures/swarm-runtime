# Phase A Capacity Planning — AF B300 Bare-Metal Cluster

**Issue:** [#168 — Substrate sovereignty: AF bare-metal B300 cluster + stable tier migration](https://github.com/alternatefutures/admin/issues/168)
**Author:** chief-architect
**Status:** Draft — pending hardware contract confirmation from Angela
**Date:** 2026-04-21
**Phase scope:** Phase A only — B300 cluster bring-up (Q2 2026)

> **Principle:** Correctness and capacity over cost optimisation.
> Per user directive: *"we can always throw more compute at the agents if they need it."*
> Phase A sizing errs on the side of headroom. Right-sizing happens in Phase B+.

---

## Table of Contents

1. [Hardware Inventory](#1-hardware-inventory)
2. [Kimi K2.5 Deployment Spec](#2-kimi-k25-deployment-spec)
3. [Ray Serve Topology](#3-ray-serve-topology)
4. [vLLM Configuration](#4-vllm-configuration)
5. [FHE Accelerator Service](#5-fhe-accelerator-service)
6. [Kubernetes Layer](#6-kubernetes-layer)
7. [Benchmark Plan](#7-benchmark-plan)
8. [Phase A Exit Criteria](#8-phase-a-exit-criteria)
9. [Dependencies on Argus Phase C Attestation](#9-dependencies-on-argus-phase-c-attestation)
10. [Blocking Questions for Angela](#10-blocking-questions-for-angela)

---

## 1. Hardware Inventory

### 1.1 Known Specifications (from NVIDIA B300 public datasheet)

| Parameter | Per-Host Value | Source |
|---|---|---|
| GPU model | NVIDIA B300 (Blackwell) | NVIDIA B300 datasheet |
| GPU count per host | **TBC — see §10** | Contract |
| HBM3e per GPU | 192 GB | NVIDIA B300 datasheet |
| HBM3e bandwidth per GPU | 8 TB/s | NVIDIA B300 datasheet |
| Tensor cores | 5th-gen Blackwell tensor cores, FP4 native | NVIDIA B300 datasheet |
| NVLink generation | NVLink5 (1.8 TB/s bidirectional per GPU pair) | NVIDIA B300 datasheet |
| NVSwitch | NVSwitch4 (intra-host, full all-reduce at NVLink5 speed) | NVIDIA B300 NVL72/HGX spec |
| CPU SKU | **TBC — see §10** | Contract |
| System RAM | **TBC — see §10** | Contract |
| Local NVMe | **TBC — see §10** | Contract |
| PCIe generation | PCIe Gen6 (host↔GPU) | NVIDIA B300 datasheet |

### 1.2 Inter-Host Fabric — OPEN QUESTION (critical path)

This is a blocking question for Angela (§10, Q-2). Two options in scope:

| Option | Bandwidth | Latency | Notes |
|---|---|---|---|
| **400GbE** (RoCEv2) | 400 Gb/s per link; 800 Gb/s dual-port | ~1–2 µs | Lower cost; RDMA over Converged Ethernet; widely available |
| **InfiniBand NDR** | 400 Gb/s per link (NDR400) | ~100–200 ns | Lower latency, better all-reduce; higher cost; preferred for multi-node tensor parallel |

**Preliminary recommendation:** IB NDR for the inference node ring if the lease budget allows. Multi-node pipeline parallelism across B300 hosts at 128k+ context lengths will saturate 400GbE under high fan-out. If cost prohibits IB NDR, accept 400GbE RoCEv2 with ECMP bonding (2×400GbE = 800 Gb/s effective) and adjust PP degree (§2.2).

### 1.3 Assumed Minimum Viable Cluster (Phase A)

Pending Angela's contract numbers, planning will proceed on two scenarios:

| Scenario | Hosts | GPUs total | Total HBM3e | Notes |
|---|---|---|---|---|
| **Conservative (4-host)** | 4 | 32 | 6 TB | Minimum for 2-node TP + PP; FHE on same ring |
| **Target (8-host)** | 8 | 64 | 12 TB | Preferred; leaves dedicated FHE hosts |

All sizing in §2–§5 uses **Target (8-host, 64 GPU)** as the planning baseline. Sections flag Conservative-scenario adjustments where material.

### 1.4 Power and Cooling

- B300 TDP: ~1,400 W per GPU at peak (B300 SXM5 class)
- 8-host × 8 GPU/host × 1.4 kW = **89.6 kW GPU draw** before CPU/DRAM/NVMe overhead
- Total estimated rack draw: ~110–130 kW
- **Blocking:** PDU and cooling capacity must be confirmed in the lease contract (§10, Q-3)

---

## 2. Kimi K2.5 Deployment Spec

> Builds on inference cluster design doc (PR #139, issue #132). This section translates that design to B300 hardware specifics.

### 2.1 Quantization Strategy

B300 Blackwell GPUs support **FP4 (MX-FP4, microscaling format)** natively via the 5th-gen tensor cores. This is the primary reason B300 was selected for Kimi K2.5 inference.

| Format | Memory per 1B params | Throughput vs FP16 | Accuracy |
|---|---|---|---|
| FP16 (baseline) | ~2 GB/B | 1× | Reference |
| BF16 | ~2 GB/B | ~1× | Reference |
| FP8 (E4M3) | ~1 GB/B | ~2× | <0.5% quality delta |
| **FP4 (MX-FP4)** | **~0.5 GB/B** | **~4×** | Acceptable for chat/agent tasks; monitor on STEM-heavy evals |

**Decision: Deploy Kimi K2.5 in FP4 (MX-FP4) on B300.**

Kimi K2.5 is a MoE model. Activated parameter count at inference time is a fraction of total parameter count, making FP4 quality loss less impactful than for dense models.

**Rationale:** FP4 on B300 is not an optimisation — it is the native inference format for this hardware. Running at FP8 or BF16 would cut throughput by 2–4× and waste the primary hardware advantage. Accept FP4 with eval-gated rollout (see §7).

### 2.2 Tensor Parallelism (TP) and Pipeline Parallelism (PP)

Kimi K2.5 parameter count: publicly estimated at ~1T total, ~32B activated per token (MoE, top-K routing). Exact shard count is TBC pending Moonshot AI license terms (OQ-A from PR #139).

**Intra-host (NVLink5 ring):** TP=8 per host. All 8 GPUs on a single host are connected at NVLink5 speeds (full all-reduce ≤ 10 µs at 1.8 TB/s). This is the zero-latency parallel axis.

**Inter-host (IB NDR or 400GbE):** PP across hosts. Pipeline stage boundaries cross the inter-host fabric — this is the latency-sensitive axis.

| Scenario | TP | PP | Hosts used | GPUs used | HBM3e allocated | Notes |
|---|---|---|---|---|---|---|
| **128k context, OPEN pool** | 8 | 2 | 2 | 16 | 3,072 GB | Full-speed; fits on 2 hosts |
| **1M context, OPEN pool** | 8 | 4 | 4 | 32 | 6,144 GB | Extended KV cache; PP=4 cross-host |
| **RESTRICTED (separate hosts)** | 8 | 2 | 2 | 16 | 3,072 GB | Dedicated hosts, never shared |
| **FHE service (§5)** | — | — | 2 | 16 | 3,072 GB | GPU TFHE kernels, not inference TP |

**Conservative-scenario adjustment (4 hosts):** OPEN and RESTRICTED cannot both run at full PP=4 simultaneously. RESTRICTED gets TP=8 PP=1 on 1 host (fits within 192GB×8=1.5TB for FP4 activated params); OPEN gets TP=8 PP=2 on 2 hosts; FHE shares remaining host.

### 2.3 Expected Throughput at 128k and 1M Context

These are engineering estimates based on B300 spec projections and Blackwell-class benchmarks. **Must be validated against actual hardware in Phase A benchmarks (§7).**

| Context length | Batch size | Est. TTFT | Est. decode (tok/s/GPU) | Est. throughput (req/s, 8-GPU TP) | Notes |
|---|---|---|---|---|---|
| 4k (baseline) | 32 | <50 ms | ~8,000 | ~200 | FP4, KV cache hot |
| 32k | 16 | <200 ms | ~4,000 | ~80 | KV cache growth |
| **128k** | **8** | **<500 ms** | **~1,500** | **~25** | Target for agent tasks |
| **1M** | **2** | **<4 s** | **~300** | **~3** | Long-context agents; PP=4 required |

Phase A acceptance target: **128k context at ≥20 req/s sustained** on the OPEN pool. 1M context at ≥2 req/s sustained.

### 2.4 KV Cache Sizing

KV cache namespace scheme (from PR #138, §5.2):

```
{tenant_id}:{sensitivity_level}:{agent_type}:{prefix_hash}
```

Where `sensitivity_level` ∈ {`OPEN`, `SENSITIVE`, `RESTRICTED`}. RESTRICTED tasks **do not share KV cache** (PR #138, §5.3).

**KV cache memory budget per pool:**

| Pool | Hosts | HBM3e | Model weights (FP4 est.) | KV cache budget | Notes |
|---|---|---|---|---|---|
| OPEN | 2 hosts × 8 GPU | 3,072 GB | ~1,600 GB (est.) | **~1,400 GB** | Cross-tenant prefix sharing allowed |
| RESTRICTED | 2 hosts × 8 GPU | 3,072 GB | ~1,600 GB | **~1,400 GB** | Per-tenant isolation, no cross-tenant sharing |

**KV cache layout (vLLM PagedAttention — §4.1):**

Each KV page = configurable block size (default 16 tokens). With 128k context and 1,400 GB budget:

- Pages per pool: `1,400 GB / (block_size_bytes)` — exact sizing in §4
- Prefix cache hit-rate target: ≥70% for common system prompt prefixes in OPEN pool
- RESTRICTED pool: no cross-tenant prefix sharing; per-tenant prefix cache keyed by `{tenant_id}:{prefix_hash}`

### 2.5 MIG Partitioning — Rejected

**Decision: Do not use MIG on B300 for Phase A.**

Rationale:
1. MIG partitions a single GPU into fixed-size instances. For large LLM inference (Kimi K2.5), the model requires all 192 GB HBM3e per GPU for weights + KV cache. Sub-partitioning eliminates this headroom.
2. MIG does not cross GPU boundaries. TP=8 across all GPUs on a host requires NVLink-connected full GPUs, which MIG disrupts.
3. Pool isolation (OPEN vs RESTRICTED) is handled at the host level (separate physical hosts), making MIG-based isolation redundant and counterproductive.
4. MIG is appropriate for small-model multi-tenancy (e.g., embedding models) that AF does not run on this cluster in Phase A.

If a separate embedding or fine-tune inference cluster is added in Phase B+, revisit MIG for those workloads.

---

## 3. Ray Serve Topology

> Extends inference cluster design (PR #139). This section adds host-level placement constraints imposed by bare-metal B300 topology and RESTRICTED host separation.

### 3.1 Head Node Placement

- **One dedicated head node** — not a GPU host. A CPU-only node (or a single GPU host without inference workloads) runs:
  - Ray head process
  - Ray Dashboard
  - GCS (Global Control Store)
  - Autoscaler
- **Rationale:** Mixing Ray head process with GPU inference workloads causes head GCS failures under GPU OOM pressure. Dedicated head is standard production practice.
- **Minimum spec for head node:** 16 vCPU, 64 GB RAM, 500 GB NVMe. This may be a separate smaller host in the lease or a CPU-only VM — flag to Angela (§10, Q-5).

### 3.2 Worker Pools per Sensitivity Level

RESTRICTED hosts must be physically separate from OPEN hosts. This is a hard requirement from PR #138 §3.1 (three-layer fail-closed enforcement).

```
┌─────────────────────────────────────────────────────────────────────┐
│  AF B300 Cluster — Ray Serve Worker Pool Layout                     │
│                                                                     │
│  ┌──────────────┐   NVLink5   ┌──────────────┐                     │
│  │ Host A       │◄───────────►│ Host B       │  OPEN POOL          │
│  │ 8× B300 GPU  │             │ 8× B300 GPU  │  (Kimi K2.5 TP=8   │
│  │ OPEN label   │             │ OPEN label   │   PP=2 across A+B)  │
│  └──────────────┘             └──────────────┘                     │
│                                                                     │
│  ┌──────────────┐   NVLink5   ┌──────────────┐                     │
│  │ Host C       │◄───────────►│ Host D       │  RESTRICTED POOL    │
│  │ 8× B300 GPU  │             │ 8× B300 GPU  │  (physically        │
│  │ RESTR label  │             │ RESTR label  │   separate hosts)   │
│  └──────────────┘             └──────────────┘                     │
│                                                                     │
│  ┌──────────────┐   NVLink5   ┌──────────────┐                     │
│  │ Host E       │◄───────────►│ Host F       │  FHE POOL           │
│  │ 8× B300 GPU  │             │ 8× B300 GPU  │  (TFHE-rs GPU       │
│  │ FHE label    │             │ FHE label    │   kernels — §5)     │
│  └──────────────┘             └──────────────┘                     │
│                                                                     │
│  ┌──────────────┐   ┌──────────────┐                               │
│  │ Host G       │   │ Head Node    │  BURST / HEAD                 │
│  │ 8× B300 GPU  │   │ CPU-only     │  (G = OPEN burst + 1M ctx)   │
│  │ OPEN-BURST   │   │              │                               │
│  └──────────────┘   └──────────────┘                               │
└─────────────────────────────────────────────────────────────────────┘
```

**Kubernetes node labels:**

```yaml
# OPEN pool nodes
node.af.ai/pool: open
node.af.ai/sensitivity: open

# RESTRICTED pool nodes
node.af.ai/pool: restricted
node.af.ai/sensitivity: restricted

# FHE pool nodes
node.af.ai/pool: fhe
node.af.ai/sensitivity: restricted  # FHE processes RESTRICTED data
```

**Node taints** (hard anti-affinity):

```yaml
# On RESTRICTED nodes:
taints:
  - key: "af.ai/pool"
    value: "restricted"
    effect: NoSchedule

# Ray Serve RESTRICTED deployments carry matching toleration
# OPEN pool deployments explicitly do NOT carry this toleration
```

### 3.3 Autoscaling Configuration

```yaml
# OPEN pool — Ray Serve deployment
autoscaling_config:
  min_replicas: 2          # Always-warm; no cold-start on agent burst
  max_replicas: 12         # Up to 3 full TP=8 TP groups across OPEN hosts
  initial_replicas: 2
  target_num_ongoing_requests_per_replica: 8
  upscale_delay_s: 10      # Fast upscale on agent fan-out
  downscale_delay_s: 120   # Slow downscale; avoid thrash during long agent sessions
  metrics_interval_s: 5

# RESTRICTED pool — Ray Serve deployment
autoscaling_config:
  min_replicas: 1
  max_replicas: 4          # Hard cap: only 2 RESTRICTED hosts (16 GPUs)
  initial_replicas: 1
  target_num_ongoing_requests_per_replica: 4
  upscale_delay_s: 15
  downscale_delay_s: 300   # Long delay; RESTRICTED sessions are longer
```

### 3.4 Health Checks

```yaml
# vLLM backend health
liveness_probe:
  path: /health
  initial_delay_s: 120     # B300 model load time (FP4 Kimi K2.5 weights)
  period_s: 15
  failure_threshold: 3

readiness_probe:
  path: /v1/models
  initial_delay_s: 120
  period_s: 10
  failure_threshold: 2

# Ray Serve actor health
health_check_period_s: 15
health_check_timeout_s: 30
graceful_shutdown_timeout_s: 30   # Drain in-flight requests before kill
graceful_shutdown_wait_loop_timeout_s: 180
```

### 3.5 Burst-to-Bare-Metal Policy (Atlas coordination point)

Atlas is OPEN POOL DRI. The following burst-to-bare-metal policy applies when Akash OPEN POOL is saturated:

1. Akash worker pods emit a `pool.capacity.critical` NATS event when queue depth > threshold.
2. Ray Serve autoscaler detects `max_replicas` reached on Akash-backed pool.
3. Overflow routes to AF B300 OPEN POOL via the model router (PR #139, §4.1).
4. **Atlas owns the burst threshold and routing weights** — not this document.
5. This spec guarantees the AF OPEN POOL is always ready (min_replicas: 2) to receive burst traffic without cold-start.

**Interface point:** Atlas (PR #132/#139) must define the `pool.capacity.critical` event schema and the model router weight update mechanism. Chief-architect owns the AF B300 pool readiness contract; Atlas owns the Akash-side burst trigger.

---

## 4. vLLM Configuration

> Aligns with prefix cache namespace scheme `{tenant_id}:{sensitivity_level}:{agent_type}:{prefix_hash}` from PR #138 §5.2.

### 4.1 PagedAttention Settings

```python
# vLLM server startup — OPEN pool
vllm serve moonshot-ai/kimi-k2.5 \
  --dtype fp4 \                        # Native B300 FP4 inference
  --tensor-parallel-size 8 \           # Full NVLink5 ring on single host
  --pipeline-parallel-size 2 \         # Across 2 hosts for 128k ctx
  --max-model-len 131072 \             # 128k context
  --gpu-memory-utilization 0.90 \      # Reserve 10% for CUDA overhead
  --block-size 32 \                    # 32-token KV pages (larger = better HBM utilisation for long ctx)
  --max-num-seqs 256 \                 # Max concurrent sequences
  --max-num-batched-tokens 32768 \     # Continuous batch budget
  --enable-prefix-caching \           # Enable PagedAttention prefix cache
  --prefix-cache-max-blocks 65536 \   # ~2 GB prefix cache index at 32-token blocks
  --served-model-name kimi-k2-5-open \
  --port 8000

# For 1M context variant (Host G burst pool):
vllm serve moonshot-ai/kimi-k2.5 \
  --dtype fp4 \
  --tensor-parallel-size 8 \
  --pipeline-parallel-size 4 \         # 4-host PP chain for 1M ctx
  --max-model-len 1048576 \
  --gpu-memory-utilization 0.92 \
  --block-size 32 \
  --max-num-seqs 8 \                   # Fewer concurrent at 1M; KV cache dominates
  --enable-prefix-caching \
  --served-model-name kimi-k2-5-long
```

### 4.2 Prefix Cache Alignment with Namespace Scheme

vLLM's prefix cache is keyed by token hash. To align with `{tenant_id}:{sensitivity_level}:{agent_type}:{prefix_hash}`:

1. **System prompt injection:** The inference client (PR #139, `crates/af-inference-client/`) prepends a structured prefix token block:
   ```
   [BOS] <af:tenant:{tenant_id}> <af:level:{sensitivity_level}> <af:agent:{agent_type}>
   ```
   This ensures tenant-scoped system prompts hash to a unique prefix, enabling cache hits across same-tenant, same-agent requests.

2. **RESTRICTED pool — no cross-tenant prefix sharing:** vLLM prefix cache is enabled on RESTRICTED pool, but the structured prefix ensures tenant_id is always part of the hash root. Two tenants with identical system prompts will never share a cached block (the tenant_id prefix diverges at block 1).

3. **SENSITIVE tier:** Treated as OPEN for prefix caching purposes (no FHE; standard cache). The `{sensitivity_level}` token in the prefix ensures SENSITIVE and OPEN requests do not share cached blocks.

4. **Cache eviction:** LRU. Per PR #138, eviction must not leak tenant data — vLLM's block-level memory management ensures evicted pages are zeroed before reuse. Validate this in Phase A security check.

### 4.3 Continuous Batching Parameters

```yaml
# Target: maximise MFU (Model FLOPs Utilisation) on B300
scheduler_config:
  max_num_batched_tokens: 32768    # Single-pass prefill budget
  max_num_seqs: 256                # Decode concurrency
  max_paddings: 256                # Tolerate sparse batches at tail latency

# Chunked prefill (vLLM ≥0.4.0)
enable_chunked_prefill: true
max_num_chunked_prefill_tokens: 8192  # Chunk long prefills to reduce TTFT variance
```

**Rationale for chunked prefill:** Without chunked prefill, a 128k-token prefill blocks the decode queue for the entire prefill duration. Chunked prefill interleaves prefill chunks with decode steps, capping worst-case TTFT variance at the cost of ~5% throughput reduction. At agent fan-out scale, tail latency stability is worth this trade.

### 4.4 Speculative Decoding Posture

**Decision: Disabled for Phase A.**

Rationale:
1. Kimi K2.5 is MoE. Draft model selection for speculative decoding on MoE requires a matching small MoE draft model, which is not available at Phase A.
2. FP4 MoE on B300 is already near memory-bandwidth-bound; speculative decoding adds complexity for uncertain gain on this hardware class.
3. Revisit in Phase B when throughput baseline on AF hardware is established.

If in Phase A benchmarks (§7) decode throughput falls below target (§7.2), speculative decoding with a small dense draft model (e.g., Kimi-mini or a fine-tuned 7B) will be re-evaluated as the first tuning lever.

---

## 5. FHE Accelerator Service

### 5.1 TFHE-rs GPU Kernel Feasibility on B300

**Current state (as of April 2026):**
TFHE-rs (Zama) supports GPU acceleration via CUDA. The GPU-accelerated path was introduced for Hopper (H100) architecture and has been validated on sm_90. B300 is sm_100 (Blackwell). GPU kernel compatibility needs validation.

| Kernel target | Status | Notes |
|---|---|---|
| TFHE-rs CUDA (sm_90 / Hopper) | Production | Ships in TFHE-rs ≥0.7 |
| TFHE-rs CUDA (sm_100 / Blackwell) | **Unconfirmed** | Must validate; expect sm_90 PTX JIT to work but no native sm_100 kernels yet |
| TFHE-rs CUDA B300 native | Future | Likely in TFHE-rs 2026 H2 roadmap post-Blackwell public release |

**Recommendation:** Proceed with B300 FHE service using sm_90 PTX JIT compilation on sm_100 (Blackwell). This will work but will not capture sm_100 tensor core advantages. Native sm_100 TFHE-rs kernels are a Phase A stretch goal, not a requirement.

### 5.2 Expected `fhe_search` Latency — Sub-1s Target

Current CPU TFHE-rs baseline (H100 GPU accelerated for reference):
- H100 GPU: ~50–100ms per 256-bit FHE gate (single bootstrapping op)
- B300 GPU (est. via sm_90 PTX JIT): ~30–70ms, scaling with HBM3e bandwidth

**`fhe_search` operation decomposition:**

A typical `fhe_search` query involves:
1. FHE-encrypted query embedding (client-side, not on-server)
2. FHE distance computation over encrypted index (server-side, GPU)
3. FHE result decryption (client-side)

The server-side step (2) is the target for sub-1s. This requires:
- Batching: Process N search queries per GPU kernel invocation (amortise bootstrapping overhead)
- Batch size: 32 concurrent queries → est. 200–400ms total for step (2) at 32 queries/batch on 8× B300

**Target:** `fhe_search` server-side latency ≤ 500ms at batch_size=32 on 2-host FHE pool (16× B300 GPUs).
**End-to-end target** (including network round-trip): ≤ 1s P99.

### 5.3 FHE Service Architecture

```
                    ┌─────────────────────────────────┐
                    │  FHE Accelerator Service         │
                    │  (Rust, tonic gRPC)              │
                    │                                  │
                    │  ┌──────────────────────────┐   │
  gRPC FheSearch ──►│  │ BatchQueue               │   │
  (encrypted payload)│  │ max_batch=32             │   │
                    │  │ timeout=10ms (fill wait) │   │
                    │  └──────────┬───────────────┘   │
                    │             │                    │
                    │  ┌──────────▼───────────────┐   │
                    │  │ TFHE-rs GPU Executor     │   │
                    │  │ 16× B300 GPUs (2 hosts)  │   │
                    │  │ CUDA stream pool         │   │
                    │  └──────────┬───────────────┘   │
                    │             │                    │
                    │  ┌──────────▼───────────────┐   │
                    │  │ CPU Fallback             │   │
                    │  │ (same bare-metal hosts)  │   │
                    │  │ activated if GPU OOM/err │   │
                    │  └──────────────────────────┘   │
                    └─────────────────────────────────┘
```

**gRPC interface** (extends PR #139 `crates/af-inference-client/` proto):

```protobuf
service FheAcceleratorService {
  rpc FheSearch(FheSearchRequest) returns (FheSearchResponse);
  rpc FheBatch(FheBatchRequest) returns (FheBatchResponse);
  rpc HealthCheck(HealthCheckRequest) returns (HealthCheckResponse);
}

message FheSearchRequest {
  bytes encrypted_query = 1;   // TFHE ciphertext
  string tenant_id = 2;
  string index_id = 3;
  uint32 top_k = 4;
}
```

### 5.4 CPU Fallback Plan

If GPU TFHE kernels do not meet latency target or fail to validate on B300:

1. **Trigger condition:** GPU FHE P99 > 800ms OR GPU kernel crash rate > 0.1% in 10-minute rolling window.
2. **Fallback action:** FHE service routes new requests to CPU TFHE-rs workers on the same bare-metal hosts.
3. **CPU performance:** B300 hosts with modern server CPUs (TBC, §10) will run TFHE-rs CPU at ~2–5s per `fhe_search` — outside the <1s target but functional.
4. **Fallback SLA:** Phase A passes with CPU fallback *if* CPU latency is ≤ 5s P99 and GPU path is unblocked within the Phase A window.
5. **Phase A exit criterion adjustment:** If CPU fallback is active at Phase A exit, this becomes a Phase B hard dependency — GPU FHE path must reach ≤1s before Phase B stable tier migration begins.

### 5.5 FHE Prototype Plan

| Milestone | Deliverable | Exit Criteria |
|---|---|---|
| M1: Environment validation | TFHE-rs builds + CUDA toolkit on B300 | `cargo test` passes with GPU feature flag; sm_90 PTX JIT validates |
| M2: Single-request benchmark | `fhe_search` latency on single B300 GPU | ≤ 500ms median on single GPU, batch=1 |
| M3: Batch benchmark | `fhe_search` at batch=32 across 8 GPUs | ≤ 500ms median; ≤ 800ms P99 |
| M4: Service integration | gRPC service live; connected to af-inference-client | End-to-end `fhe_search` call from RESTRICTED agent succeeds |
| M5: Load test | 100 concurrent RESTRICTED agents using `fhe_search` | P99 ≤ 1s under load; CPU fallback not triggered |

**Exit criteria for GPU path (before CPU fallback is acceptable as permanent):** M3 must pass. If M3 fails by Phase A exit date, CPU fallback is declared for Phase A with a committed timeline for GPU path in Phase B.

---

## 6. Kubernetes Layer

### 6.1 k3s vs Full Kubernetes

**Decision: Full Kubernetes (k8s), not k3s.**

| Factor | k3s | Full k8s | Winner |
|---|---|---|---|
| Bare-metal complexity | Low (single binary) | Higher (kubeadm/kubespray) | k3s |
| GPU Operator support | Partial (community) | Full (NVIDIA official) | k8s |
| Node count (8 hosts) | Either works | Either works | Tie |
| NVIDIA GPU Operator | Works but less tested | Fully supported | k8s |
| Cilium eBPF CNI | Works | Works (better tested) | k8s |
| Production SLA | Limited HA options | Full HA control plane | k8s |
| AF team familiarity | Unknown | Unknown | Tie |

**Rationale:** The primary driver is NVIDIA GPU Operator support. k3s community support for GPU Operator lags the official NVIDIA k8s support by typically 2–3 minor versions. Given B300 is new hardware requiring the latest GPU Operator, using full k8s eliminates a support gap that could block Phase A by weeks.

Deployment method: **kubeadm** on bare-metal, with `etcd` on the head node (or dedicated etcd cluster if head node count ≥ 3 — TBC with Angela).

### 6.2 NVIDIA GPU Operator Version

- **Target: GPU Operator v24.x (latest stable at Phase A bring-up)**
- Minimum: GPU Operator v23.9.x (Blackwell B300 initial support)
- Validate: `nvidia-device-plugin`, `nvidia-container-toolkit`, `dcgm-exporter` all load on B300 with driver ≥ 560

```yaml
# GPU Operator Helm values
operator:
  defaultRuntime: containerd
  validator:
    driver:
      env:
        - name: DISABLE_DEV_CHAR_SYMLINK_CREATION
          value: "true"

driver:
  version: "560.35.03"   # Minimum for B300; pin to tested version

mig:
  strategy: none    # MIG disabled (§2.5)

toolkit:
  version: "1.16.0"
```

### 6.3 CNI — Cilium

**Decision: Cilium (eBPF-based CNI).**

Rationale:
1. eBPF observability: per-pod network flow visibility without sidecar overhead — essential for auditing OPEN/RESTRICTED traffic isolation.
2. NetworkPolicy enforcement at eBPF level (kernel bypass) reduces latency vs iptables for inference traffic.
3. Cilium's `CiliumNetworkPolicy` (L7-aware) supports the OPEN/RESTRICTED isolation enforcement from PR #138 §3.3.
4. Cilium Hubble provides flow-level audit log that integrates with the SEC-00x event schema (PR #138 §8).

```yaml
# Cilium Helm values (key settings)
cilium:
  kubeProxyReplacement: "strict"   # Full kube-proxy bypass
  bpf:
    masquerade: true
    hostRouting: true
  hubble:
    enabled: true
    relay:
      enabled: true
    ui:
      enabled: true
  nodePort:
    enabled: true
  bandwidthManager:
    enabled: true   # BBR congestion control for inference traffic
```

**Critical NetworkPolicy for RESTRICTED isolation:**

```yaml
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: restricted-pool-isolation
  namespace: af-inference
spec:
  endpointSelector:
    matchLabels:
      af.ai/pool: restricted
  ingress:
  - fromEndpoints:
    - matchLabels:
        af.ai/role: restricted-gateway   # Only the RESTRICTED gateway may connect
  egress:
  - toEndpoints:
    - matchLabels:
        af.ai/pool: restricted           # RESTRICTED pods may only reach other RESTRICTED pods
    - matchLabels:
        af.ai/role: fhe-service          # ...or the FHE service
```

### 6.4 Storage Classes

| Data type | Access pattern | Storage class | Backing |
|---|---|---|---|
| KV cache / prefix cache | Hot, byte-addressed, ephemeral | `local-nvme-fast` | Local NVMe on inference host (no replication) |
| vLLM model weights | Read-only, load-on-start | `local-nvme-weights` | Local NVMe, pre-pulled at node provision time |
| SurrealDB L2 state | Random read/write, durable | `local-nvme-durable` | Local NVMe + WAL replication (Phase B) |
| NATS JetStream | Sequential write, replicated | `local-nvme-durable` | Same as SurrealDB (Phase B) |
| Checkpoint / cold state | Write-once, DR | `s3-glacier` | AWS S3 + Glacier (off-cluster) |

```yaml
# local-nvme-fast StorageClass (KV cache — ephemeral)
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: local-nvme-fast
provisioner: kubernetes.io/no-provisioner
volumeBindingMode: WaitForFirstConsumer
reclaimPolicy: Delete   # KV cache is ephemeral; reclaim on pod deletion
```

```yaml
# local-nvme-durable StorageClass (state — survives pod restart)
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: local-nvme-durable
provisioner: kubernetes.io/no-provisioner
volumeBindingMode: WaitForFirstConsumer
reclaimPolicy: Retain   # State must survive pod restart
```

**KV cache pod spec (PVC vs hostPath):** Use `hostPath` volumes for KV cache, not PVCs. PVC provisioning adds scheduling latency that is unacceptable for hot inference cache. The vLLM process owns the KV cache memory buffer directly (PagedAttention GPU memory pool), not a filesystem. The NVMe `local-nvme-fast` class is used for prefix cache persistence (hash→block index), not for the in-GPU KV buffer.

---

## 7. Benchmark Plan

### 7.1 Workload Definitions

| Workload ID | Description | Tool | Notes |
|---|---|---|---|
| BM-01 | vLLM TTFT, 4k context, batch 32 | vLLM benchmark_serving.py | Baseline |
| BM-02 | vLLM TTFT, 128k context, batch 8 | vLLM benchmark_serving.py | Primary target |
| BM-03 | vLLM throughput, 128k context, sustained | vLLM benchmark_throughput.py | 10-min sustained run |
| BM-04 | vLLM TTFT, 1M context, batch 2 | vLLM benchmark_serving.py | Long-context agents |
| BM-05 | Ray Serve tail latency, agent fan-out 100 concurrent | locust or k6 | Simulate 100 agents all calling inference simultaneously |
| BM-06 | Ray Serve tail latency, agent fan-out 1000 concurrent | locust or k6 | Scale ceiling test |
| BM-07 | FHE `fhe_search` latency, batch 1 | custom harness (TFHE-rs bench) | Single-request baseline |
| BM-08 | FHE `fhe_search` latency, batch 32 | custom harness | Batch target |
| BM-09 | FHE `fhe_search` load test, 100 concurrent RESTRICTED agents | locust | End-to-end FHE SLA |
| BM-10 | KV prefix cache hit rate, shared system prompt | vLLM metrics endpoint | Validate §4.2 |
| BM-11 | OPEN/RESTRICTED network isolation | Cilium Hubble flow capture | Security validation |
| BM-12 | GPU memory utilisation at max load | DCGM exporter metrics | Headroom validation |

### 7.2 Target Metrics and Acceptance Criteria

| Metric | Target | Accept Threshold | Fail Condition |
|---|---|---|---|
| BM-02: TTFT, 128k, batch 8 | < 500ms median | < 700ms P95 | P95 > 1s |
| BM-03: Sustained throughput, 128k | ≥ 20 req/s | ≥ 15 req/s | < 10 req/s |
| BM-04: TTFT, 1M context | < 4s median | < 6s P95 | P95 > 10s |
| BM-05: Ray Serve P99, 100 agents | < 1s | < 1.5s | > 2s |
| BM-06: Ray Serve P99, 1000 agents | < 3s | < 5s | > 8s |
| BM-07: FHE latency, batch 1 | < 200ms | < 400ms | > 500ms |
| BM-08: FHE latency, batch 32 | < 500ms | < 800ms P99 | > 1s P99 |
| BM-09: FHE end-to-end, 100 agents | < 1s P99 | < 1.5s P99 | > 2s P99 |
| BM-10: KV prefix cache hit rate | ≥ 70% | ≥ 50% | < 30% |
| BM-11: Isolation — OPEN→RESTRICTED | 0 cross-pool flows | 0 | Any cross-pool flow |
| BM-12: GPU memory at peak load | ≤ 90% HBM3e | ≤ 92% | > 95% |

### 7.3 Benchmark Execution Plan

Phase A benchmark sprint runs over 2 weeks after cluster bring-up:

| Week | Activity |
|---|---|
| Bring-up+1 | BM-01, BM-07, BM-11, BM-12 (smoke tests) |
| Bring-up+2 | BM-02, BM-03, BM-04, BM-08 (primary benchmarks) |
| Bring-up+3 | BM-05, BM-06, BM-09, BM-10 (scale + integration benchmarks) |

All results published to `docs/infrastructure/benchmark-results/phase-a/` with raw logs and summary tables.

---

## 8. Phase A Exit Criteria

These are binary pass/fail. **All must pass for Phase A to be declared complete and Phase B migration to begin.**

| # | Criterion | Pass | Fail action |
|---|---|---|---|
| EC-01 | Cluster is fully provisioned: all hosts running, GPU Operator healthy, Cilium CNI operational | All nodes in `Ready` state; all GPU pods running | Block; fix provisioning |
| EC-02 | Kimi K2.5 model loaded in FP4 on OPEN pool | vLLM `/v1/models` returns kimi-k2-5-open; no OOM at startup | Block; check memory |
| EC-03 | vLLM TTFT P95 at 128k context ≤ 700ms (BM-02) | BM-02 P95 ≤ 700ms | Block; tune TP/PP/batching |
| EC-04 | vLLM sustained throughput ≥ 15 req/s at 128k (BM-03) | BM-03 ≥ 15 req/s | Block; tune |
| EC-05 | Ray Serve P99 < 1.5s at 100 agent fan-out (BM-05) | BM-05 P99 ≤ 1.5s | Block; tune autoscaling |
| EC-06 | RESTRICTED pool runs on physically separate hosts from OPEN pool | Cilium Hubble: zero OPEN→RESTRICTED flows in 1-hour soak | Block; re-check NetworkPolicy |
| EC-07 | FHE `fhe_search` P99 ≤ 800ms batch=32 on GPU path (BM-08) | BM-08 P99 ≤ 800ms | *Conditional pass if CPU fallback < 5s (§5.4)* |
| EC-08 | FHE CPU fallback works (if GPU path fails EC-07) | 100 RESTRICTED agents complete FHE search; no timeout | Block if both GPU and CPU fail |
| EC-09 | KV prefix cache hit rate ≥ 50% (BM-10) | BM-10 ≥ 50% | Block; check namespace scheme |
| EC-10 | Akash OPEN POOL burst-to-AF handoff works | End-to-end: Akash pool saturated → request routes to AF B300 → response returned | Block; coordinate Atlas |
| EC-11 | 1M context inference operational (BM-04) | BM-04 completes without error; P95 ≤ 6s | Warning (non-blocking) if > 6s; block if OOM |
| EC-12 | DCGM metrics exported to Prometheus; Grafana dashboards live | GPU memory, temperature, MFU visible in Grafana | Block if blind to GPU health |
| EC-13 | Kimi K2.5 quality gate passes | Internal MMLU-Pro + agent task eval ≥ 95% of FP16 baseline | Block; escalate to model selection |

**Notes:**
- EC-07 has a conditional pass for CPU fallback (§5.4). This is the only criterion with a conditional.
- EC-11 is warning-non-blocking only if 1M context deployment exists but is slow. If 1M context causes OOM and the service crashes, it is blocking.
- EC-13 eval harness must be prepared before Phase A benchmarks begin. This is a preparation dependency, not a hardware dependency.

---

## 9. Dependencies on Argus Phase C Attestation

Phase C (RESTRICTED POD on AF confidential compute, TDX/SEV-SNP) runs in parallel with Phase A. The following interface points must be agreed between chief-architect (Phase A) and Argus (Phase C) before Phase B stable tier migration begins.

### 9.1 Interface Points

| # | Interface | Phase A responsibility | Phase C (Argus) responsibility |
|---|---|---|---|
| IP-01 | **Inference pool vs RESTRICTED pool host separation** | Chief-architect guarantees physical host separation (§3.2). OPEN hosts have no TDX/SEV capability required. | Argus defines which hosts will run RESTRICTED POD with TDX/SEV-SNP attestation. These will be a *subset* of the RESTRICTED pool hosts in §3.2. |
| IP-02 | **Attestation boot-time contract** | Phase A RESTRICTED hosts bring up without attestation (Nitro dependency removal is Phase C). Phase A RESTRICTED pool must be provisionable without TEE firmware. | Argus defines the attestation contract that RESTRICTED hosts must satisfy at Phase C cutover. Chief-architect provisions Phase A hosts with TDX/SEV firmware *capable* hardware so Phase C does not require new hosts. |
| IP-03 | **KV cache isolation for RESTRICTED attestation boundary** | KV cache namespace scheme (`{tenant_id}:{sensitivity_level}:...`) is implemented in Phase A (§2.4, §4.2). | Argus must confirm this scheme satisfies the attestation boundary for Phase C — i.e., that KV pages for RESTRICTED workloads are within the TDX TD (Trust Domain) or SEV-SNP ASID boundary. |
| IP-04 | **RESTRICTED pod admission controller** | Phase A admission controller (Kubernetes) enforces `node.af.ai/pool: restricted` node selector. | Phase C admission controller must additionally enforce attestation token validation before scheduling. Argus owns the attestation token spec; chief-architect integrates into the admission controller webhook. |
| IP-05 | **Audit log event schema** | Phase A implements SEC-00x event schema (PR #138 §8) for inference calls. | Argus Phase C adds attestation-specific events (SEC-ATT-001 etc.). Schema must be additive; no breaking changes to Phase A events. |

### 9.2 Blocking Questions for Phase C Coordination

The following must be resolved before Phase B migration begins (not Phase A exit, but Phase B gate):

1. **TDX vs SEV-SNP selection:** Argus has not yet published a recommendation (Phase C spec is in-flight). Chief-architect needs this before ordering Phase A replacement hosts — if TDX is chosen, B300 hosts must have Intel Xeon CPUs with TDX support; if SEV-SNP, AMD EPYC CPUs with SEV-SNP. (Also see §10, Q-1.)
2. **Which RESTRICTED pool hosts will be Phase C candidates:** Not all 2 RESTRICTED hosts need TDX/SEV. Argus must specify which and how many.
3. **Attestation service latency budget:** The attestation check at inference time has a latency cost. Argus must specify the expected attestation overhead so chief-architect can include it in Phase C TTFT budget.

---

## 10. Blocking Questions for Angela

The following items from the B300 lease contract are required before finalising the hardware inventory (§1) and storage configuration (§6.4). **This spec cannot be fully locked until these are answered.**

| # | Question | Why blocking | Owner |
|---|---|---|---|
| Q-1 | **CPU SKU per host** — Intel Xeon or AMD EPYC? Which generation? | Determines TDX (Intel) vs SEV-SNP (AMD) feasibility for Phase C RESTRICTED hosts | Angela → chief-architect |
| Q-2 | **Inter-host fabric** — 400GbE RoCEv2 or InfiniBand NDR? What switch count / topology (leaf-spine)? | Determines PP degree for multi-host inference (§2.2) and whether 1M context PP=4 is viable at target latency | Angela → chief-architect |
| Q-3 | **GPU count per host** — 8× B300 SXM5 (NVL72 rack config) or different count? | Drives all TP sizing. Spec assumes 8× per host; if different, §2.2 tables must be recalculated | Angela → chief-architect |
| Q-4 | **Local NVMe capacity and count per host** | KV cache and weight storage sizing (§6.4) depends on NVMe total | Angela → chief-architect |
| Q-5 | **Is there a CPU-only head node in the lease, or must we use a GPU host as head?** | Affects Ray head node placement (§3.1) and wastes GPU capacity if a GPU host is used as head | Angela → chief-architect |
| Q-6 | **System RAM per host** | Affects CPU fallback FHE capacity (§5.4) and OS/driver memory headroom | Angela → chief-architect |
| Q-7 | **PDU capacity and cooling (kW) per rack** | Safety gate: 8 hosts × ~15kW/host ≈ 120kW. Must not exceed PDU rating | Angela → facilities |
| Q-8 | **Delivery and rack-ready date** | Phase A timeline (Q2 2026) depends on hardware availability | Angela → chief-architect |
| Q-9 | **Kimi K2.5 commercial license terms** (OQ-A from PR #139) | Production inference deployment requires license clarity | Angela + legal → chief-architect |

---

## References

- [Issue #168 — Substrate sovereignty](https://github.com/alternatefutures/admin/issues/168) *(this document tracks Phase A only)*
- [PR #139 — Shared inference cluster design (Ray Serve + vLLM + Kimi K2.5)](https://github.com/alternatefutures/admin/pull/139) — upstream design this spec instantiates on B300 hardware
- [PR #138 — Security design (RESTRICTED POD, FHE boundary, TEE.fail)](https://github.com/alternatefutures/admin/pull/138) — RESTRICTED host separation, KV namespace scheme, NetworkPolicy
- [PR #143 — Durable state schema (Turso L1 + SurrealDB L2)](https://github.com/alternatefutures/admin/pull/143) — OQ-9 state tier decisions; do not deviate
- [PR #151 — OQ-9 tiered state security addendum (Argus)](https://github.com/alternatefutures/admin/pull/151) — Argus input on Turso/SurrealDB security posture
- [Issue #133 — Security design](https://github.com/alternatefutures/admin/issues/133) — FHE boundary context
- [Issue #132 — Shared inference cluster design](https://github.com/alternatefutures/admin/issues/132) — inference cluster context
- [Issue #129 — EPIC: Rust Swarm Runtime 1000-agent](https://github.com/alternatefutures/admin/issues/129) — parent epic
- NVIDIA B300 Architecture Whitepaper (public, April 2026)
- TFHE-rs GPU acceleration docs — Zama (zama.ai/tfhe-rs)
- vLLM documentation — PagedAttention, chunked prefill, prefix caching
- Cilium documentation — CiliumNetworkPolicy, Hubble observability
- Ray Serve documentation — deployment autoscaling, head node configuration
