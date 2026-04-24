# AF Swarm Runtime — Test and Benchmark Plan

**Document:** `docs/architecture/design/test-benchmark-plan.md`
**Status:** DRAFT — Design Phase
**Author:** QA Engineer
**Date:** 2026-04-16
**Issue:** [alternatefutures/admin#135](https://github.com/alternatefutures/admin/issues/135)
**Epic:** [#129](https://github.com/alternatefutures/admin/issues/129) — 1000-Agent Rust Swarm Runtime
**RFC Ground Truth:** [`1000-agent-amendment.md`](../reviews/1000-agent-amendment.md) (PR #128)
**v3.0 Plan Reference:** [`rust-swarm-runtime.md`](../rust-swarm-runtime.md) — DO NOT MODIFY
**Amends:** Does NOT modify the RFC or v3.0 plan. This is an additive design document.

**Coordinates with:**
- **Lain (#131)** — runtime internals design: supervision tree, lifecycle state machine, backpressure limits
- **Atlas (#132)** — inference cluster design: vLLM config, batch sizes, KV cache, autoscaling parameters
- **Argus (#133)** — security design: RESTRICTED POD routing enforcement chain, KV namespace isolation
- **Chief Architect (#136)** — integration contracts: Rust runtime → inference cluster gRPC proto

---

## 0. Purpose and Scope

This document defines the test and benchmark strategy for the 1000-agent Rust Swarm Runtime, covering everything needed to validate the performance claims in RFC §1 and identify failure modes before production.

### Claims Being Validated

| Claim | Source | Test Section |
|-------|--------|-------------|
| 1,000+ concurrent agents supported | RFC §1.1 | §3 Scale Test |
| <50ms agent cold start (Dapr hydration) | RFC §1.2 | §4.1 |
| ~80ns L1 Flume message latency | v3.0 §3.2 | §4.2 |
| <50–80µs L3 NATS cross-machine latency | v3.0 §3.2 | §4.3 |
| Centralized topology: 4.4× error amplification | RFC §1.4 | §5 |
| Pure P2P topology: 17.2× error amplification (baseline) | RFC §1.4 | §5 |
| Kimi K2.5 emits parallel work graphs (PARL property) | RFC §1.3 | §6 |
| ≤16 tools/agent trade-off holds in our system | RFC §1.4, §1.6 | §7 |
| FHE overhead at RESTRICTED tier is acceptable | RFC §4, v3.0 §5.4 | §8 |
| Six-layer security stack maintains invariants under shared inference | RFC §4 | §5 + §9 |

### What This Document Covers

- Scale test methodology (§3): synthetic workload generator design, agent concurrency ramp
- Latency SLO tests (§4): cold start, warm dispatch, end-to-end round-trip
- Error amplification measurement (§5): topology comparison, injection and tracking methodology
- PARL verification (§6): Kimi K2.5 parallel graph analysis
- Tool-coordination stress test (§7): 16 vs 32 tools, overhead measurement
- FHE overhead benchmark (§8): inference latency on/off FHE, CPU vs GPU
- Failure injection plan (§9): Dapr crash, state store partition, OOM, agent panic, orchestrator timeout
- Chaos framework choice and configuration (§10)
- CI gate matrix (§11): per-PR / nightly / release-only
- Observability to prove SLOs: Prometheus queries (§12), Grafana dashboard spec (§13), alert rules (§14)

### What This Document Does NOT Cover

- Frontend, Svelte, React, or any web-app code
- Full implementation of test harnesses (skeleton code and type definitions only)
- GPU hardware procurement (see RFC OQ-3)
- Exact vLLM batch-size configuration (deferred to Atlas, PR #132)
- Agent TOML schema (Lain, PR #131)
- Security enforcement implementation (Argus, PR #133)

---

## 1. Testing Philosophy

### 1.1 Core Principles

1. **No mocks for inference.** All tests that involve model calls use a real inference endpoint. For local dev/CI: small local vLLM instance serving Qwen2.5-0.5B-Instruct (cheap, fast, open-source). For scale tests requiring Kimi K2.5: dedicated Akash GPU provider or staging cluster. See §3.4 for infrastructure requirements.

2. **Fail-closed on observability.** If a Prometheus scrape target is unavailable, the test fails. We do not run scale tests blind.

3. **Measure what the RFC claims, not proxies.** The 4.4× error amplification number must be measured on our system, not borrowed from the Google/MIT paper. The paper's methodology is a reference; our measurement is the ground truth for our topology.

4. **Chaos is not optional.** Failure injection is a first-class test concern, not a post-launch activity. Every failure mode in the MAST taxonomy (v3.0 §5, 18 modes + 2 added by RFC §7 compatibility matrix) has a corresponding test.

5. **GPU-gated tests are explicitly tagged.** Tests requiring GPU are tagged `#[cfg_attr(not(feature = "gpu-tests"), ignore)]`. They do not break CI on standard runners. See §11 for CI gate matrix.

### 1.2 Test Naming Conventions

```
tests/
├── unit/                   # fast, no I/O, per-PR always
├── integration/
│   ├── messaging/          # Flume/NATS tests, no inference
│   ├── lifecycle/          # agent hydrate/freeze cycles, Dapr required
│   ├── inference/          # requires local vLLM
│   └── security/           # DSE routing, FHE, RESTRICTED POD
├── scale/                  # 1000-agent load tests, nightly
├── chaos/                  # failure injection, nightly
└── benchmarks/             # criterion micro-benchmarks, per-PR
    ├── messaging/
    ├── lifecycle/
    └── fhe/
```

### 1.3 Feature Flags

```toml
# Cargo.toml
[features]
default = []
gpu-tests = []          # enables tests requiring CUDA GPU
dapr-tests = []         # enables tests requiring live Dapr sidecar
nats-tests = []         # enables tests requiring live NATS server
scale-tests = []        # enables 1000-agent scale tests (nightly only)
inference-tests = []    # enables tests requiring live vLLM endpoint
chaos-tests = []        # enables chaos/failure injection tests
```

---

## 2. Test Infrastructure Requirements

### 2.1 Environments

| Environment | Where | Used For | GPU? |
|-------------|-------|---------|------|
| **Dev local** | Developer laptop | Unit tests, criterion benchmarks | No |
| **CI baseline** | GitHub Actions (standard runner, 4 vCPU/16GB) | Unit tests, integration tests (mock inference), criterion benchmarks | No |
| **CI inference** | GitHub Actions + Akash CPU provider | Integration tests with real Qwen2.5-0.5B vLLM (CPU-only, slow but real) | No |
| **CI nightly GPU** | Akash GPU provider (A10 or equivalent) | Scale tests, FHE GPU benchmarks, Kimi K2.5 PARL tests | Yes |
| **Staging** | Akash multi-node (mirrors production topology) | Release-gate tests, full 1K agent tests, chaos tests | Yes (multi-node) |

### 2.2 Local vLLM for Non-GPU CI

For tests that need a real inference endpoint but not GPU:

```bash
# Run Qwen2.5-0.5B on CPU (slow but functional — only for correctness tests)
docker run --rm -p 8000:8000 \
  -e VLLM_CPU_ONLY=1 \
  vllm/vllm-openai:latest \
  --model Qwen/Qwen2.5-0.5B-Instruct \
  --dtype float32 \
  --device cpu \
  --max-model-len 4096 \
  --max-num-seqs 4  # low concurrency for CPU
```

This is not representative of production throughput but validates request routing, response parsing, and error handling without GPU.

### 2.3 Test Harness Architecture

```rust
// tests/harness/mod.rs — skeleton
pub struct SwarmTestHarness {
    runtime: Arc<SwarmRuntime>,
    dapr_client: DaprTestClient,
    nats_client: async_nats::Client,
    vllm_endpoint: Url,
    prometheus: PrometheusTestClient,
    agent_factory: AgentFactory,
}

impl SwarmTestHarness {
    /// Spawn N agents concurrently, returning handles for task dispatch.
    pub async fn spawn_agents(&self, count: usize, config: AgentTestConfig) -> Vec<AgentHandle>;

    /// Submit a task to an agent and await completion, measuring latency.
    pub async fn dispatch_task(&self, agent: &AgentHandle, task: TestTask) -> TaskResult;

    /// Inject a failure into the system at the specified target.
    pub async fn inject_failure(&self, failure: FailureInjection);

    /// Collect current Prometheus metrics snapshot.
    pub async fn metrics_snapshot(&self) -> MetricsSnapshot;

    /// Assert that a Prometheus query result satisfies a predicate.
    pub async fn assert_metric(&self, query: &str, predicate: impl Fn(f64) -> bool);
}

pub struct AgentTestConfig {
    pub tool_count: usize,      // 0..=32 (capped at 16 in prod)
    pub data_tier: DataTier,
    pub model_tier: ModelTier,  // Quen (routing), Kimi (orchestrator), Claude (restricted)
    pub initial_state: Option<AgentState>,
}

pub enum FailureInjection {
    DaprSidecarCrash { node_id: NodeId },
    StateStorePartition { duration: Duration },
    RayServeOom { replica: String },
    AgentPanic { agent_id: AgentId },
    OrchestratorTimeout { delay: Duration },
    NatsPartition { target: NatsTarget, duration: Duration },
    FheKeyCorruption { agent_id: AgentId },
    KvCachePoison { namespace: TenantNamespace },
}
```

---

## 3. Scale Test: 1,000-Agent Concurrent Load

### 3.1 Methodology Overview

The scale test answers RFC OQ-5: "What does the 1,000-agent load test look like?" We define three tiers of scale tests, each more demanding:

| Tier | Agent Count | Task | Per-Agent Complexity | Hydration Required? | Run When |
|------|------------|------|---------------------|--------------------|---------|
| **S1: Smoke** | 100 | 10-token classification | Minimal | No (all pre-warmed) | Per-PR (nightly CI baseline) |
| **S2: Load** | 500 | 50-token summarization | Medium | Yes (50% cold start) | Nightly |
| **S3: Target** | 1,000 | 100-token task with 2 tool calls | Full | Yes (90% cold start from Dapr) | Release gate |
| **S4: Stress** | 1,500 | Same as S3 + 20% RESTRICTED mix | Full | Yes | Release gate (before 1K milestone) |

**Why 1,500 in S4?** The RFC claims 1,000+ as the minimum target. S4 demonstrates headroom — we should be able to run 1,500 before degradation begins. If we can only just barely hit 1,000, that is a red flag.

### 3.2 Synthetic Workload Generator

```rust
// tests/scale/workload_generator.rs — skeleton

pub struct WorkloadGenerator {
    agent_count: usize,
    task_profile: TaskProfile,
    ramp_strategy: RampStrategy,
    injection_rate: f64,    // tasks/second per agent
}

pub enum RampStrategy {
    /// Spawn all agents simultaneously — measures burst hydration cost.
    InstantBurst,
    /// Add agents at fixed rate — measures steady-state throughput.
    GradualRamp { agents_per_second: usize },
    /// Maintain target concurrency — replaces finished agents immediately.
    ConstantConcurrency { target: usize },
}

pub struct TaskProfile {
    pub prompt_tokens: RangeInclusive<usize>,
    pub output_tokens: RangeInclusive<usize>,
    pub tool_calls: u8,
    pub data_tier: DataTier,
    pub requires_hydration: bool,
}

impl WorkloadGenerator {
    /// Run the scale test, collecting metrics throughout.
    pub async fn run(&self, harness: &SwarmTestHarness, duration: Duration) -> ScaleTestReport {
        // 1. Ramp agents according to strategy
        // 2. Dispatch tasks at injection_rate per agent
        // 3. Collect: throughput, latency percentiles, hydration times, error rates
        // 4. Return structured report
        todo!()
    }
}

pub struct ScaleTestReport {
    pub peak_concurrent_agents: usize,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub hydration_latency: LatencyHistogram,    // cold start times
    pub dispatch_latency: LatencyHistogram,     // warm path times
    pub e2e_latency: LatencyHistogram,          // full round-trip
    pub error_rate: f64,
    pub throughput: f64,                        // tasks/second
    pub p99_within_slo: bool,
}
```

### 3.3 Metrics Captured During Scale Tests

Every scale test run collects:

```
# Agent counts
swarm_active_agents_total{state="hydrating|active|draining|freezing|frozen"}
swarm_agents_hydrated_total
swarm_agents_frozen_total
swarm_agents_quarantined_total

# Task throughput
swarm_tasks_dispatched_total{tier="public|internal|sensitive|restricted"}
swarm_tasks_completed_total{status="success|error|timeout"}
swarm_tasks_in_flight

# Hydration latency (cold start)
swarm_agent_hydration_duration_seconds{bucket=...}

# Dispatch latency (warm path)
swarm_agent_dispatch_duration_seconds{tier="l1|l2|l3", bucket=...}

# Inference latency (cluster side)
vllm_request_latency_seconds{model="kimi-k25|qwen-0.5b|claude", bucket=...}
vllm_batch_size_current{deployment="kimi-k25|qwen-0.5b"}

# Resource utilization
swarm_dapr_state_store_ops_total{op="read|write", status="ok|error"}
swarm_memory_working_bytes{agent_id=...}  # MemAgent working set
```

### 3.4 Infrastructure for Scale Tests

Scale tests (S2+) **require** a live Dapr sidecar and NATS server. Scale test S3+ requires GPU for Kimi K2.5:

```yaml
# ops/scale-test-infra/docker-compose.scale.yml — skeleton
services:
  dapr-placement:
    image: daprio/dapr:1.13
    command: ["./placement", "-port", "50005"]

  redis-state:
    image: redis:7-alpine
    # Used as Dapr state store backend for agent state

  nats:
    image: nats:2.10-alpine
    command: ["-js"]  # JetStream enabled

  prometheus:
    image: prom/prometheus:v2.50.0
    volumes:
      - ./prometheus-scale.yml:/etc/prometheus/prometheus.yml

  # For S1/S2: CPU vLLM (Qwen only)
  vllm-cpu:
    image: vllm/vllm-openai:latest
    environment:
      VLLM_CPU_ONLY: "1"
    # ... (see §2.2)

  # For S3+: GPU vLLM (requires --gpus flag or Akash GPU provider)
  # Kimi K2.5 — see Atlas (#132) for exact config
```

**Note for Akash deployment:** S3 and S4 tests cannot run on standard Akash CPU providers. They require a GPU provider. The Akash deployment for scale testing is tracked separately from production infrastructure. See RFC OQ-3 (who owns inference cluster infrastructure?).

### 3.5 Pass/Fail Criteria

| Test Tier | Required Condition | SLO Reference |
|-----------|-------------------|---------------|
| S1 (100 agents) | P99 dispatch latency ≤ 50ms; 0% task failure | §4 SLOs |
| S2 (500 agents) | P99 dispatch latency ≤ 100ms; ≤ 0.1% failure; P99 hydration ≤ 200ms | §4 SLOs |
| S3 (1,000 agents) | P99 dispatch ≤ 200ms; ≤ 0.5% failure; P99 hydration ≤ 500ms; throughput ≥ 500 tasks/s | §4 SLOs |
| S4 (1,500 agents) | System degrades gracefully (no panic cascade); P99 dispatch ≤ 500ms | headroom test only |

---

## 4. Latency SLO Tests

### 4.1 Cold Start (Agent Hydration from Dapr State)

**What it measures:** Time from "task arrives for a frozen agent" to "agent is active and processing." This is the Dapr hydration time — the state-load round-trip.

**RFC claim:** Sub-50ms (RFC §1.2 — from Dapr's published sub-50ms cold start target for virtual actors).

**Our system SLO:**
- P50 hydration: ≤ 20ms
- P95 hydration: ≤ 50ms  (matches RFC claim)
- P99 hydration: ≤ 200ms (allows for state store contention under load)
- P99.9 hydration: ≤ 500ms (absolute ceiling per RFC OQ-5 answer)

**Test design:**

```rust
#[tokio::test]
#[cfg_attr(not(feature = "dapr-tests"), ignore)]
async fn test_cold_start_latency() {
    let harness = SwarmTestHarness::new_with_dapr().await;

    // Pre-populate agent state in Dapr (simulate previously frozen agent)
    let agent_id = harness.freeze_agent_with_state(test_agent_state()).await;

    // Measure hydration time under varying state sizes
    for state_size_kb in [1, 10, 100, 500] {
        let state = generate_agent_state(state_size_kb * 1024);
        harness.write_dapr_state(&agent_id, &state).await;

        let start = Instant::now();
        let handle = harness.hydrate_agent(&agent_id).await.unwrap();
        let hydration_time = start.elapsed();

        assert!(
            hydration_time <= Duration::from_millis(200),
            "P99 cold start budget exceeded: {:?} for {}KB state",
            hydration_time, state_size_kb
        );

        harness.freeze_agent(handle).await;
    }
}

#[tokio::test]
#[cfg_attr(not(feature = "dapr-tests"), ignore)]
async fn test_cold_start_latency_under_load() {
    let harness = SwarmTestHarness::new_with_dapr().await;

    // Freeze 200 agents, then hydrate them concurrently
    let agents: Vec<AgentId> = harness.freeze_n_agents(200).await;

    let start = Instant::now();
    let results: Vec<_> = futures::future::join_all(
        agents.iter().map(|id| harness.hydrate_agent(id))
    ).await;

    let times: Vec<Duration> = results.into_iter()
        .map(|r| r.expect("hydration failed"))
        .map(|h| h.hydration_duration)
        .collect();

    let p99 = percentile(&times, 99.0);
    assert!(p99 <= Duration::from_millis(500), "P99 cold start under 200-concurrent load: {:?}", p99);

    harness.metrics_snapshot().await
        .assert_histogram_p99("swarm_agent_hydration_duration_seconds", 0.5);
}
```

**[OPEN QUESTION OQ-TB-1]** The Dapr docs cite "sub-50ms cold start" but that assumes a local Redis state store with no contention. Our production state store may be a remote Redis or PostgreSQL. The 50ms SLO needs to be validated against the actual state store latency budget. Should the 50ms SLO be P50 or P95? RFC OQ-5 says "P99 hydration <500ms" as the baseline; this plan sets P95 ≤ 50ms as a tighter target but that may need revision after Atlas (#132) finalizes the state store backend choice.

### 4.2 Warm Dispatch (L1 Flume — In-Process)

**What it measures:** Message latency for agent-to-agent communication within the same runtime process (L1 path — Flume MPMC channels).

**RFC claim / v3.0 target:** ~80ns primitive channel latency. Chief Architect note: end-to-end local message latency (including routing and actor mailbox processing) will be ~500ns–2µs.

**Our system SLO:**
- Flume primitive send: ≤ 200ns (measured by criterion)
- End-to-end L1 (agent A → router → agent B): ≤ 5µs (P99)
- L1 throughput: ≥ 1M messages/second (single router, 10 agents)

**Benchmark (criterion):**

```rust
// benches/messaging/l1_flume.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};

fn bench_flume_send(c: &mut Criterion) {
    let mut group = c.benchmark_group("l1_flume");

    for msg_size in [64, 256, 1024] {
        group.bench_with_input(
            BenchmarkId::new("send_recv", msg_size),
            &msg_size,
            |b, &size| {
                let (tx, rx) = flume::bounded::<Vec<u8>>(256);
                let msg = vec![0u8; size];
                b.iter(|| {
                    tx.send(black_box(msg.clone())).unwrap();
                    black_box(rx.recv().unwrap());
                });
            },
        );
    }

    // Native Rust type (no serialization) — the hot path
    group.bench_function("agent_message_native", |b| {
        let (tx, rx) = flume::bounded::<AgentMessage>(256);
        let msg = AgentMessage::test_task();
        b.iter(|| {
            tx.send(black_box(msg.clone())).unwrap();
            black_box(rx.recv().unwrap());
        });
    });

    group.finish();
}

fn bench_l1_end_to_end(c: &mut Criterion) {
    // Includes L1RouterActor dispatch table lookup and mailbox delivery
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("l1_end_to_end");

    group.bench_function("agent_a_to_agent_b_10_agents", |b| {
        let harness = rt.block_on(SwarmTestHarness::new_minimal(10));
        b.to_async(&rt).iter(|| async {
            black_box(
                harness.send_l1_message(AgentId::from(0), AgentId::from(1), AgentMessage::test_task())
                    .await
                    .unwrap()
            )
        });
    });

    group.finish();
}

criterion_group!(benches, bench_flume_send, bench_l1_end_to_end);
criterion_main!(benches);
```

### 4.3 Cross-Machine Latency (L3 NATS)

**What it measures:** Round-trip time for agent-to-agent message via NATS (cross-node, cross-machine).

**RFC claim / v3.0 target:** ~50–80µs.

**Our system SLO:**
- L3 one-way: ≤ 100µs (P99, same datacenter)
- L3 round-trip: ≤ 200µs (P99, same datacenter)

```rust
// benches/messaging/l3_nats.rs
fn bench_nats_roundtrip(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let nats_url = std::env::var("NATS_URL").unwrap_or("nats://localhost:4222".to_string());

    c.bench_function("nats_publish_roundtrip", |b| {
        let client = rt.block_on(async_nats::connect(&nats_url)).unwrap();
        let subject = "bench.roundtrip";
        let payload = Bytes::from(vec![0u8; 64]);

        b.to_async(&rt).iter(|| async {
            let start = Instant::now();
            client.publish(subject, payload.clone()).await.unwrap();
            // ... await subscription response
            black_box(start.elapsed())
        });
    });
}
```

### 4.4 End-to-End Round-Trip (Inference Included)

**What it measures:** Full latency from task submission to result returned, including inference cluster round-trip.

**SLOs (RFC §1.2 cold start + inference budget):**

| Path | P50 | P95 | P99 |
|------|-----|-----|-----|
| Warm agent + Qwen classifier (10-token) | ≤ 100ms | ≤ 200ms | ≤ 500ms |
| Warm agent + Kimi K2.5 (100-token task) | ≤ 500ms | ≤ 1s | ≤ 2s |
| Cold agent + Kimi K2.5 (100-token task) | ≤ 600ms | ≤ 1.5s | ≤ 2.5s |
| RESTRICTED + Claude API (50-token task) | ≤ 1s | ≤ 3s | ≤ 5s |

**Note:** The RESTRICTED path latency budget is higher because it routes to Claude API (external, rate-limited). This is expected and acceptable per RFC §1.5.

**[OPEN QUESTION OQ-TB-2]** The inference latency budget depends on Atlas's (#132) vLLM configuration — specifically `max_num_batched_tokens`, KV cache size, and continuous batching parameters. The SLOs above are initial targets; they should be reviewed and potentially revised after Atlas finalizes the cluster design. The test infrastructure must accept the SLO values as parameters, not hardcode them.

---

## 5. Error Amplification Measurement

### 5.1 Why We Must Measure This on Our System

The RFC cites the Google/MIT Mar 2026 study: centralized topology achieves 4.4× error amplification vs peer-to-peer's 17.2×. These numbers come from their test configuration and agent implementations. We cannot assume the ratio holds identically for our system — error propagation behavior depends on our specific orchestrator behavior, agent retry logic, and task dependency graph structure.

We must reproduce an equivalent experiment on our topology and report our measured amplification factor. If our factor differs materially from 4.4×, we document it as an open question rather than silently accept the published number.

### 5.2 Error Amplification Definition

**Error amplification factor** = (tasks that produce incorrect outputs due to one upstream error) / (direct errors injected)

Example: inject 1 hallucination into one agent → measure how many total tasks in the DAG produce incorrect results as a consequence.

### 5.3 Test Design

#### Setup

Two topologies are tested side-by-side:

| Topology | Configuration | Expected Amplification |
|----------|--------------|----------------------|
| **Centralized** (our target) | Kimi K2.5 orchestrator at root; all tasks enter via orchestrator | 4.4× (RFC claim) |
| **Flat P2P** (baseline for comparison) | No orchestrator; agents communicate directly peer-to-peer | 17.2× (RFC claim) |

The flat P2P topology is not our production design — it is the baseline against which centralized is measured. We must implement a test-only flat P2P mode to get the comparative measurement.

#### Error Injection Mechanism

```rust
/// Wraps an AgentActor to inject a single hallucination at a configured trigger.
pub struct ErrorInjector {
    target: AgentId,
    trigger: InjectionTrigger,
    error_type: InjectedError,
    injected: AtomicBool,
}

pub enum InjectionTrigger {
    OnNthTask(usize),
    OnTaskWithKeyword(String),
    OnFirstTask,
}

pub enum InjectedError {
    /// Replace model output with incorrect content.
    Hallucination { wrong_answer: String },
    /// Return a structurally valid but semantically wrong tool result.
    WrongToolResult { tool: String, wrong_value: serde_json::Value },
    /// Return an error instead of completing the task.
    TaskFailure { reason: String },
}
```

#### Measurement Procedure

```rust
#[tokio::test]
#[cfg_attr(not(all(feature = "scale-tests", feature = "inference-tests")), ignore)]
async fn test_error_amplification_centralized_topology() {
    let harness = SwarmTestHarness::new_centralized(100).await;  // 100 agents, centralized

    // 1. Define a multi-step task graph with known correct outputs
    let task_graph = ErrorAmplificationTaskGraph::new()
        .with_upstream_tasks(5)      // 5 independent tasks
        .with_downstream_tasks(20);  // 20 tasks that depend on upstream results

    // 2. Establish baseline: run task graph with no errors, verify all outputs correct
    let baseline = harness.run_task_graph(&task_graph).await;
    assert_eq!(baseline.incorrect_outputs, 0, "baseline must be clean");

    // 3. Inject one error into one upstream agent
    harness.inject_error(ErrorInjector {
        target: task_graph.upstream_agents[0],
        trigger: InjectionTrigger::OnFirstTask,
        error_type: InjectedError::Hallucination {
            wrong_answer: task_graph.known_wrong_answer.clone(),
        },
        injected: AtomicBool::new(false),
    }).await;

    // 4. Run same task graph, count incorrect outputs (downstream tasks that got wrong input)
    let with_error = harness.run_task_graph(&task_graph).await;

    let amplification_factor = with_error.incorrect_outputs as f64 / 1.0;  // 1 injected error
    println!("Centralized topology error amplification: {:.2}×", amplification_factor);

    // 5. Assert it is in the expected range (RFC claims 4.4× for centralized)
    // We allow ±50% variance to account for task graph structure differences
    assert!(
        amplification_factor >= 1.0 && amplification_factor <= 10.0,
        "Centralized amplification {:.2}× is outside expected range [1.0, 10.0]. RFC claims ~4.4×",
        amplification_factor
    );

    // Record in Prometheus for trending
    harness.record_gauge("swarm_error_amplification_factor", amplification_factor,
        &[("topology", "centralized")]);
}

#[tokio::test]
#[cfg_attr(not(all(feature = "scale-tests", feature = "inference-tests")), ignore)]
async fn test_error_amplification_flat_p2p_baseline() {
    // Same test but with flat P2P topology for comparison
    let harness = SwarmTestHarness::new_flat_p2p(100).await;
    // ... (identical procedure)
    // Expected amplification > centralized; documents the advantage
}
```

#### Reporting

The test outputs:
```
Error Amplification Report
  Centralized topology:   X.X× (RFC claims: 4.4×, Google/MIT 2026)
  Flat P2P topology:      Y.Y× (RFC claims: 17.2×, Google/MIT 2026)
  Our improvement ratio:  Y.Y÷X.X = Z.Z× reduction with centralized orchestrator
```

If the centralized factor is materially worse than 4.4× (e.g., >10×), this is surfaced as an **open question** and does not automatically fail the build — the topology choice is still valid even if the exact number differs, but we must not cite the 4.4× claim in our own documentation without our measured value.

**[OPEN QUESTION OQ-TB-3]** The Google/MIT study's task graphs had a specific structure (depth, branching, dependency patterns). Our "error amplification task graph" design must be documented as a separate test fixture. The measured factor will depend on the graph structure. If our factor is significantly different from 4.4×, this should be raised as a review question on RFC §1.4 before the RFC is marked ACCEPTED.

---

## 6. PARL Verification: Kimi K2.5 Parallel Graph Emission

### 6.1 What We Are Testing

RFC §1.3 claims Kimi K2.5 is "trained via PARL with `r_parallel` reward that specifically penalizes serial-collapse in multi-agent graphs." The key testable prediction: **given a parallelizable task, K2.5 emits a parallel work graph, not a sequential chain**.

We cannot inspect model weights or training reward signals, but we can measure the output: does the orchestrator's task planning produce parallel vs sequential DAGs when given tasks that admit parallelism?

### 6.2 Test Tasks (Parallelism Oracle Set)

A set of canonical tasks where the correct parallel decomposition is known:

```rust
/// Tasks with known optimal parallel structure.
/// For each task, we know: (a) can it be parallelized? (b) what is the optimal parallel factor?
pub const PARL_ORACLE_TASKS: &[ParallelismOracleTask] = &[
    ParallelismOracleTask {
        description: "Summarize 5 independent blog posts",
        prompt: "Summarize each of the following 5 blog posts: [post1], [post2], [post3], [post4], [post5]",
        // Optimal: 5 parallel summarization agents, 1 merge agent
        min_parallel_factor: 3,  // at least 3 should be parallelized
        expected_serial_collapse_rate: 0.05,  // ≤5% of the time it should emit serial steps
    },
    ParallelismOracleTask {
        description: "Research 3 independent topics",
        prompt: "Research: (1) Dapr actor model, (2) vLLM PagedAttention, (3) PARL training",
        min_parallel_factor: 2,
        expected_serial_collapse_rate: 0.1,
    },
    ParallelismOracleTask {
        description: "Multi-agent code review",
        prompt: "Review this PR: security analysis, performance analysis, test coverage analysis",
        min_parallel_factor: 3,
        expected_serial_collapse_rate: 0.05,
    },
    // ... more oracle tasks
];

/// A task that CANNOT be parallelized — sequential by nature.
/// K2.5 should NOT emit parallel steps for these.
pub const SEQUENTIAL_ORACLE_TASKS: &[SequentialOracleTask] = &[
    SequentialOracleTask {
        description: "Multi-step reasoning with dependency chain",
        prompt: "Step 1: calculate X. Step 2: using X, calculate Y. Step 3: using Y, decide Z.",
        max_parallel_factor: 1,  // all steps are sequential
    },
    // ...
];
```

### 6.3 Graph Structure Analysis

```rust
pub struct TaskGraphAnalyzer;

impl TaskGraphAnalyzer {
    /// Analyzes a task DAG emitted by the orchestrator.
    /// Returns the parallelism characteristics.
    pub fn analyze(dag: &TaskDag) -> GraphAnalysisResult {
        GraphAnalysisResult {
            total_steps: dag.nodes.len(),
            max_parallel_width: dag.max_parallel_width(),  // max nodes at same depth level
            critical_path_length: dag.critical_path().len(),
            parallel_factor: dag.max_parallel_width() as f64 / dag.critical_path().len() as f64,
            is_serial_collapse: dag.max_parallel_width() == 1,  // all steps are sequential
            independence_ratio: dag.count_independent_nodes() as f64 / dag.nodes.len() as f64,
        }
    }
}

pub struct GraphAnalysisResult {
    pub total_steps: usize,
    pub max_parallel_width: usize,
    pub critical_path_length: usize,
    pub parallel_factor: f64,
    pub is_serial_collapse: bool,
    pub independence_ratio: f64,
}
```

### 6.4 PARL Test Suite

```rust
#[tokio::test]
#[cfg_attr(not(all(feature = "inference-tests")), ignore)]
async fn test_kimi_k25_parl_parallelism() {
    let harness = SwarmTestHarness::new_with_inference(InferenceTarget::KimiK25).await;

    let mut serial_collapse_count = 0;
    let total_runs = 50;  // run each oracle task 50 times for statistical confidence

    for oracle_task in PARL_ORACLE_TASKS {
        let mut parallel_factors = Vec::new();

        for _ in 0..total_runs {
            let dag = harness.plan_task_dag(oracle_task.prompt).await;
            let analysis = TaskGraphAnalyzer::analyze(&dag);

            if analysis.is_serial_collapse {
                serial_collapse_count += 1;
            }

            parallel_factors.push(analysis.parallel_factor);
        }

        let serial_collapse_rate = serial_collapse_count as f64 / total_runs as f64;
        let mean_parallel_factor: f64 = parallel_factors.iter().sum::<f64>() / parallel_factors.len() as f64;

        println!(
            "Task: {} | Parallel factor: {:.2} | Serial collapse rate: {:.1}%",
            oracle_task.description, mean_parallel_factor, serial_collapse_rate * 100.0
        );

        assert!(
            serial_collapse_rate <= oracle_task.expected_serial_collapse_rate + 0.05,
            "Serial collapse rate {:.1}% exceeds threshold {:.1}% for task: {}",
            serial_collapse_rate * 100.0,
            oracle_task.expected_serial_collapse_rate * 100.0,
            oracle_task.description
        );

        assert!(
            mean_parallel_factor >= oracle_task.min_parallel_factor as f64,
            "Mean parallel factor {:.2} below minimum {} for task: {}",
            mean_parallel_factor, oracle_task.min_parallel_factor, oracle_task.description
        );
    }
}

#[tokio::test]
#[cfg_attr(not(feature = "inference-tests"), ignore)]
async fn test_kimi_k25_respects_sequential_constraints() {
    // Verify K2.5 does NOT incorrectly parallelize sequential tasks
    let harness = SwarmTestHarness::new_with_inference(InferenceTarget::KimiK25).await;

    for oracle_task in SEQUENTIAL_ORACLE_TASKS {
        let dag = harness.plan_task_dag(oracle_task.prompt).await;
        let analysis = TaskGraphAnalyzer::analyze(&dag);

        assert!(
            analysis.max_parallel_width <= oracle_task.max_parallel_factor,
            "K2.5 incorrectly parallelized a sequential task: {} (parallel width: {})",
            oracle_task.description, analysis.max_parallel_width
        );
    }
}
```

**[OPEN QUESTION OQ-TB-4]** The PARL test uses the Kimi K2.5 model's task planning output. This requires the orchestrator API (defined in Chief Architect's integration contract, PR #136) to expose the raw task DAG, not just the results. Confirm with Chief Architect (#136) that the orchestrator interface includes `plan_task_dag(prompt: &str) -> TaskDag` or equivalent introspection.

---

## 7. Tool-Coordination Stress Test

### 7.1 Background

RFC §1.4, citing Google/MIT: "tool-coordination overhead becomes dominant past 16 tools per agent — agents spend more tokens on tool selection and coordination than on task execution."

We need to validate that this trade-off holds in our system with our tool registry. The ≤16 cap is enforced by the resource governor (v3.0 §5.2, S6), but we should measure the actual overhead at different tool counts to confirm the cap is at the right threshold.

### 7.2 Test Design

```rust
#[tokio::test]
#[cfg_attr(not(feature = "inference-tests"), ignore)]
async fn test_tool_count_coordination_overhead() {
    let tool_counts = [4, 8, 12, 16, 20, 24, 32];
    let mut results = Vec::new();

    for &tool_count in &tool_counts {
        let harness = SwarmTestHarness::new_with_tool_count(tool_count).await;

        // Fixed task: always requires exactly 2 specific tools (same tools across all runs)
        // Extra tools are "available but irrelevant" — measures selection overhead
        let task = ToolCoordinationTask {
            description: "Read a file and grep for a pattern",
            required_tools: &["fs_read", "grep"],
            available_but_irrelevant_tools: tool_count - 2,
        };

        let mut metrics = Vec::new();
        for _ in 0..20 {  // 20 runs per tool count for averaging
            let result = harness.dispatch_task_with_metrics(&task).await;
            metrics.push(ToolCoordinationMetrics {
                prompt_tokens_used: result.prompt_tokens,
                tool_selection_tokens: result.tool_selection_tokens,  // tokens spent on tool choice reasoning
                task_execution_tokens: result.task_execution_tokens,
                total_latency: result.latency,
                correct_tools_chosen: result.tools_used == task.required_tools,
            });
        }

        let avg_selection_tokens = metrics.iter().map(|m| m.tool_selection_tokens).sum::<u64>() / metrics.len() as u64;
        let avg_total_tokens = metrics.iter().map(|m| m.prompt_tokens_used).sum::<u64>() / metrics.len() as u64;
        let selection_overhead_pct = avg_selection_tokens as f64 / avg_total_tokens as f64 * 100.0;
        let accuracy = metrics.iter().filter(|m| m.correct_tools_chosen).count() as f64 / metrics.len() as f64;

        println!(
            "Tools: {:2} | Selection overhead: {:.1}% | Accuracy: {:.0}% | Avg latency: {:?}",
            tool_count, selection_overhead_pct, accuracy * 100.0,
            Duration::from_millis(metrics.iter().map(|m| m.total_latency.as_millis() as u64).sum::<u64>() / metrics.len() as u64)
        );

        results.push((tool_count, selection_overhead_pct, accuracy));
    }

    // Verify the 16-tool threshold holds for our system
    // RFC claim: overhead becomes dominant past 16 tools
    let overhead_at_16 = results.iter().find(|(tc, _, _)| *tc == 16).unwrap().1;
    let overhead_at_32 = results.iter().find(|(tc, _, _)| *tc == 32).unwrap().1;

    println!("Tool overhead at 16: {:.1}%, at 32: {:.1}%", overhead_at_16, overhead_at_32);

    // The ≤16 cap is justified if: (a) overhead increases significantly past 16
    // OR (b) accuracy drops past 16. Either condition validates the cap.
    let overhead_growth_past_16 = overhead_at_32 - overhead_at_16;
    let accuracy_at_16 = results.iter().find(|(tc, _, _)| *tc == 16).unwrap().2;
    let accuracy_at_32 = results.iter().find(|(tc, _, _)| *tc == 32).unwrap().2;

    assert!(
        overhead_growth_past_16 > 5.0 || (accuracy_at_16 - accuracy_at_32) > 0.05,
        "16-tool cap is not justified: overhead growth past 16 is {:.1}%, accuracy drop is {:.1}%",
        overhead_growth_past_16, (accuracy_at_16 - accuracy_at_32) * 100.0
    );
}
```

**[OPEN QUESTION OQ-TB-5]** Measuring "tool selection tokens" requires the inference cluster to return per-step token attribution, not just total token count. This needs to be in the inference request/response proto (Chief Architect, PR #136). If the proto does not include this, we fall back to measuring total latency and token count only (less precise but still useful).

---

## 8. FHE Overhead Benchmark

### 8.1 Scope

FHE overhead is measured in two contexts:
1. **State store operations** (RESTRICTED agents): Dapr state read/write with FHE-encrypted payloads
2. **Inference requests** (RESTRICTED POD path): overhead of routing to Tier A vs OPEN POOL

**Note:** FHE cannot be applied to the inference computation itself (v3.0 §5.4 — vLLM cannot compute on FHE ciphertext). The "FHE overhead benchmark" measures the state encryption/decryption overhead, NOT inference-level FHE. The RESTRICTED POD routing adds latency from TLS overhead and potentially longer Tier A inference queues.

### 8.2 FHE State Operations

```rust
// benches/fhe/state_operations.rs

fn bench_fhe_compare(c: &mut Criterion) {
    let mut group = c.benchmark_group("fhe_compare");
    let keys = ClientKey::generate(TFHE_PARAMS);
    let server_key = keys.generate_server_key();

    let a = FheUint64::encrypt(42u64, &keys);
    let b = FheUint64::encrypt(42u64, &keys);

    group.bench_function("fhe_compare_eq_cpu", |bench| {
        bench.iter(|| {
            black_box(fhe_compare(&a, &b, CompareOp::Eq, &server_key))
        });
    });

    // GPU benchmark — only if CUDA available
    #[cfg(feature = "gpu-tests")]
    group.bench_function("fhe_compare_eq_gpu", |bench| {
        let gpu_server_key = server_key.to_gpu();
        bench.iter(|| {
            black_box(fhe_compare_gpu(&a, &b, CompareOp::Eq, &gpu_server_key))
        });
    });

    group.finish();
}

fn bench_fhe_agent_state_roundtrip(c: &mut Criterion) {
    // Simulates RESTRICTED agent: encrypt state → write to Dapr → read from Dapr → decrypt
    c.bench_function("fhe_agent_state_128kb", |b| {
        let state = AgentState::generate_test(128 * 1024);  // 128KB agent state
        let keys = ClientKey::generate(TFHE_PARAMS);

        b.iter(|| {
            // Encrypt before writing to Dapr
            let encrypted = fhe_encrypt_state(black_box(&state), &keys);
            // Decrypt after reading from Dapr
            let decrypted = fhe_decrypt_state(black_box(&encrypted), &keys);
            black_box(decrypted)
        });
    });
}
```

### 8.3 RESTRICTED POD Routing Overhead

```rust
#[tokio::test]
#[cfg_attr(not(all(feature = "inference-tests")), ignore)]
async fn test_restricted_pod_routing_overhead() {
    let harness = SwarmTestHarness::new_with_full_cluster().await;

    // Same 50-token task, routed through OPEN POOL vs RESTRICTED POD
    let task = TestTask::fixed_50_tokens();

    // Measure OPEN POOL (Kimi K2.5, Tier C)
    let open_latencies: Vec<Duration> = (0..50)
        .map(|_| async { harness.dispatch_to_open_pool(&task).await.latency })
        .collect::<FuturesOrdered<_>>()
        .collect::<Vec<_>>()
        .await;

    // Measure RESTRICTED POD (Claude API, Tier A)
    let restricted_latencies: Vec<Duration> = (0..50)
        .map(|_| async { harness.dispatch_to_restricted_pod(&task).await.latency })
        .collect::<FuturesOrdered<_>>()
        .collect::<Vec<_>>()
        .await;

    let open_p50 = percentile(&open_latencies, 50.0);
    let restricted_p50 = percentile(&restricted_latencies, 50.0);
    let overhead_ratio = restricted_p50.as_secs_f64() / open_p50.as_secs_f64();

    println!(
        "OPEN POOL p50: {:?}, RESTRICTED POD p50: {:?}, overhead: {:.1}×",
        open_p50, restricted_p50, overhead_ratio
    );

    // RESTRICTED pod should complete in ≤5s P99 (per §4.4 SLO)
    let restricted_p99 = percentile(&restricted_latencies, 99.0);
    assert!(
        restricted_p99 <= Duration::from_secs(5),
        "RESTRICTED POD P99 exceeds SLO: {:?}", restricted_p99
    );
}
```

### 8.4 Expected Results

| Operation | CPU (est.) | GPU (est.) | Target for AF |
|-----------|-----------|-----------|--------------|
| `fhe_compare` (64-bit, 1 comparison) | ~50ms | ~5ms | CPU acceptable for credential match |
| `fhe_aggregate` (1K values) | ~2s | ~100ms | GPU required at scale |
| `fhe_search` (10K vectors) | ~30s | ~200ms | GPU required; schedule CPU fallback alert |
| FHE state encrypt/decrypt (128KB) | ~200ms | ~20ms | Acceptable for RESTRICTED agents (cold start budget) |
| RESTRICTED POD vs OPEN POOL overhead | N/A | N/A | ≤5× (RESTRICTED tasks are low-volume) |

*CPU estimates from v3.0 §5.4.3. GPU estimates require actual benchmark with TFHE-rs CUDA backend.*

---

## 9. Failure Injection Plan

### 9.1 MAST Failure Mode Coverage

The v3.0 plan defines 18 MAST failure modes (FM-1 through FM-18, with FM-15 through FM-18 added post-TEE.fail). The RFC §7 compatibility matrix adds FM-19 and FM-20. Each must have a corresponding test.

| FM | Name | Injection Method | Recovery Expected |
|----|------|-----------------|------------------|
| **FM-1** | Agent timeout | `tokio::time::sleep` past timeout + task still pending | Orchestrator retries on different agent |
| **FM-2** | Agent panic | `panic!()` in AgentActor handler (via `ErrorInjector`) | Supervisor restarts; state checkpoint used |
| **FM-3** | Tool failure | Mock tool returning `Err` | Agent error handling; task fails with context |
| **FM-4** | Memory overflow | MemAgent eviction with corrupted state | LRU eviction continues; corrupted slot discarded |
| **FM-5** | DAG deadlock | Circular dependency injection in task graph | Orchestrator cycle detection; task rejected |
| **FM-6** | Context overflow | Inject oversized context (>max_model_len) | Chunked context splitting via ContextSharder |
| **FM-7** | Tool poisoning | Tool returns prompt injection payload | S3 injection defense; canary token detection |
| **FM-8** | Cascade failure | Kill 50% of active agents simultaneously | Dapr hydrates from state; orchestrator reroutes |
| **FM-9** | State corruption | Corrupt Dapr state store key for one agent | Emergency freeze rejected; last-good-checkpoint used |
| **FM-10** | Model API unavailable | Block outbound to Anthropic/K2.5 | Fallback model path (Claude → K2.5 or vice versa) |
| **FM-11** | NATS partition | Toxiproxy: disconnect L3 bridge | L1/L2 continue; L3 queues; reconnect on recovery |
| **FM-12** | Token budget exceeded | Agent reaches `tokens_per_task` limit | Task terminated gracefully; budget counter updated |
| **FM-13** | Injection canary triggered | Canary token appears in agent output | Agent quarantined; incident logged |
| **FM-14** | ZK verification failure | Corrupt ZK receipt | Task rejected with audit log entry |
| **FM-15** | Attestation forgery | Invalid attestation quote presented | Multi-party quorum rejects; RESTRICTED task denied |
| **FM-16** | Bus interposition | (Simulated) attestation forge via invalid PCE key | Quorum M-of-N fails; fallback to FHE-only path |
| **FM-17** | Deterministic encryption pattern analysis | (Simulated) via replaying same plaintext | Pattern detection → rotate FHE key for affected agent |
| **FM-18** | Cascading trust failure | Kill ≥(N-M+1) of M-of-N attestation verifiers | Quorum falls below threshold → RESTRICTED denials |
| **FM-19** | Inference cluster partition | Toxiproxy: disconnect Ray Serve entirely | Open tasks queue; cluster recovery auto-resume |
| **FM-20** | KV cache poisoning | Inject poisoned value into shared KV cache namespace | Tenant namespace isolation blocks cross-tenant access |

### 9.2 Injection Test Template

```rust
pub struct FailureModeTest {
    pub mode: FailureMode,
    pub setup: Box<dyn Fn(&SwarmTestHarness) -> Pin<Box<dyn Future<Output = ()>>>>,
    pub inject: Box<dyn Fn(&SwarmTestHarness) -> Pin<Box<dyn Future<Output = ()>>>>,
    pub verify_recovery: Box<dyn Fn(&SwarmTestHarness) -> Pin<Box<dyn Future<Output = RecoveryVerdict>>>>,
    pub cleanup: Box<dyn Fn(&SwarmTestHarness) -> Pin<Box<dyn Future<Output = ()>>>>,
}

pub enum RecoveryVerdict {
    RecoveredWithin(Duration),
    DegradedButFunctional { affected_agents: usize },
    FailedToRecover { reason: String },
}
```

### 9.3 Priority Injection Tests (per-PR)

These are fast (<5s), do not require GPU, and run on every PR:

1. **FM-2 (agent panic):** Single agent panics, supervisor restarts it, verify state checkpoint preserved.
2. **FM-12 (token budget):** Agent hits budget mid-task, verify graceful termination.
3. **FM-5 (DAG deadlock):** Circular dependency in task graph is detected and rejected by orchestrator.
4. **FM-7 (tool poisoning):** Canary token injected via mock tool, agent quarantined.
5. **FM-10 (model API unavailable, partial):** Block Qwen classifier → verify fallback to deterministic DSE routing.

### 9.4 Full Failure Injection Suite (Nightly)

FM-1 through FM-20, all cases. Runtime ~30 minutes with Dapr + NATS + CPU vLLM.

### 9.5 Sidecar Crash Injection

```yaml
# tests/chaos/dapr-sidecar-crash.yaml (Litmus ChaosEngine)
apiVersion: litmuschaos.io/v1alpha1
kind: ChaosEngine
metadata:
  name: dapr-sidecar-crash-test
spec:
  appinfo:
    appns: af-swarm
    applabel: "app=af-runtime"
  engineState: active
  experiments:
    - name: pod-delete
      spec:
        components:
          env:
            - name: TARGET_CONTAINER
              value: dapr-sidecar  # kill the dapr sidecar, not the runtime pod
            - name: TOTAL_CHAOS_DURATION
              value: "30"          # kill for 30 seconds
            - name: CHAOS_INTERVAL
              value: "10"
```

Expected behavior:
- Agents currently hydrated remain active (Rust runtime continues; Dapr sidecar provides state store access but not in-process messaging)
- New hydration requests fail during sidecar outage (DaprActivationListenerActor returns 503)
- After sidecar restart, hydration resumes automatically
- No in-flight task data lost (all in-process tasks complete with existing Ractor actors)

**[OPEN QUESTION OQ-TB-6]** The Dapr sidecar crash behavior depends on Lain's (#131) `DaprActivationListenerActor` design. Specifically: if the sidecar is down, can the Rust runtime continue processing tasks for already-hydrated agents without Dapr? The runtime internals design (PR #131) identifies this as OQ-RI-1 (the inversion point). The failure injection test outcome depends on that design decision. This test should be reviewed once PR #131 is merged.

---

## 10. Chaos Framework

### 10.1 Recommendation: Toxiproxy + Litmus

| Need | Tool | Why |
|------|------|-----|
| Network fault injection (latency, packet loss, disconnect) | **Toxiproxy** | Simple, Docker-friendly, no K8s needed for basic tests, programmatic API |
| K8s pod/container lifecycle chaos (OOM, sidecar crash, pod delete) | **Litmus** (LitmusChaos 3.x) | Native K8s operator, good Dapr integration, CNCF-graduated |
| Application-level fault injection (agent panics, wrong outputs) | **Custom `ErrorInjector`** (see §9.2) | Built into the Rust test harness; no external tool needed |

**Why not chaos-mesh?** Litmus and chaos-mesh have similar capabilities. Litmus is preferred because it has better documentation for Dapr sidecar targeting specifically, and we need that for FM-15 through FM-20 testing.

**Why not just Litmus for everything?** Toxiproxy does not require K8s and is much easier to run in local development and GitHub Actions CI. Network fault tests run in CI with Toxiproxy; K8s-level tests run in staging with Litmus.

### 10.2 Toxiproxy Configuration

```yaml
# tests/chaos/toxiproxy-config.yml
# Run via: docker run -d -p 8474:8474 -p 4223:4223 ghcr.io/shopify/toxiproxy

proxies:
  - name: nats
    listen: "0.0.0.0:4223"  # tests connect here instead of :4222
    upstream: "nats:4222"
    enabled: true

  - name: dapr-state-redis
    listen: "0.0.0.0:6380"
    upstream: "redis:6379"
    enabled: true

  - name: vllm-cluster
    listen: "0.0.0.0:8001"
    upstream: "vllm:8000"
    enabled: true

  - name: dapr-sidecar
    listen: "0.0.0.0:3501"
    upstream: "localhost:3500"  # Dapr HTTP port
    enabled: true
```

Programmatic control (from Rust test code):

```rust
// tests/chaos/toxiproxy_client.rs — skeleton
pub struct ToxiproxyClient {
    base_url: Url,
    http: reqwest::Client,
}

impl ToxiproxyClient {
    pub async fn add_latency(&self, proxy: &str, latency_ms: u64, jitter_ms: u64);
    pub async fn add_packet_loss(&self, proxy: &str, loss_rate: f64);
    pub async fn disconnect(&self, proxy: &str);
    pub async fn reset(&self, proxy: &str);
}
```

### 10.3 Litmus ChaosEngine Templates

```yaml
# tests/chaos/litmus/ray-serve-oom.yaml
apiVersion: litmuschaos.io/v1alpha1
kind: ChaosEngine
metadata:
  name: ray-serve-oom
spec:
  experiments:
    - name: pod-memory-hog
      spec:
        components:
          env:
            - name: TARGET_PODS
              value: "ray-serve-kimi-k25-*"
            - name: MEMORY_CONSUMPTION
              value: "90"  # hog to 90% of container limit → OOM eviction
            - name: TOTAL_CHAOS_DURATION
              value: "60"

# tests/chaos/litmus/state-store-partition.yaml
apiVersion: litmuschaos.io/v1alpha1
kind: ChaosEngine
metadata:
  name: state-store-partition
spec:
  experiments:
    - name: network-policy-chaos
      spec:
        components:
          env:
            - name: TARGET_NAMESPACE
              value: "af-swarm"
            - name: DESTINATION_IPS
              value: "redis-state:6379"  # block access to state store Redis
            - name: TOTAL_CHAOS_DURATION
              value: "120"
```

---

## 11. CI Gate Matrix

### 11.1 Per-PR (Must Pass to Merge)

**Runtime:** < 5 minutes. No GPU. No live external services (Dapr/NATS via Docker Compose test dependencies).

| Test Suite | Tags | Time | Dependencies |
|-----------|------|------|-------------|
| Unit tests (all crates) | none | ~1m | none |
| Criterion benchmarks (regression check) | none | ~2m | none |
| Integration: messaging (Flume L1/L2/L3) | `nats-tests` | ~1m | NATS Docker |
| Integration: agent lifecycle (hydrate/freeze) | `dapr-tests` | ~2m | Dapr + Redis Docker |
| Failure injection P0 (FM-2, FM-5, FM-7, FM-12, FM-10 partial) | `dapr-tests` | ~2m | Dapr + Redis Docker |
| Scale smoke test S1 (100 agents, no GPU) | `dapr-tests, scale-tests` | ~3m | Dapr + Redis + CPU vLLM |
| Security: DSE routing unit tests | none | ~30s | none |
| Security: tool count enforcement (S6 resource governor) | none | ~30s | none |

**Criterion regression gate:** If a benchmark regresses >10% vs baseline (stored in CI artifact), PR is flagged with a warning (not blocked, but visible). Regressions >25% block the PR.

### 11.2 Nightly (Runs at 02:00 UTC)

**Runtime:** ~60 minutes. Requires Dapr + NATS + CPU vLLM (Qwen2.5-0.5B).

| Test Suite | Tags | Time | Dependencies |
|-----------|------|------|-------------|
| Scale test S2 (500 agents) | `scale-tests, dapr-tests` | ~15m | Dapr + Redis + CPU vLLM |
| Full failure injection suite (FM-1 through FM-20) | `chaos-tests, dapr-tests` | ~30m | Toxiproxy + Dapr + NATS |
| PARL parallelism test (50 runs per oracle task, Qwen proxy) | `inference-tests` | ~20m | CPU vLLM |
| Tool-coordination stress test (all tool counts) | `inference-tests` | ~15m | CPU vLLM |
| Error amplification measurement (centralized + P2P) | `inference-tests, scale-tests` | ~20m | CPU vLLM |
| FHE benchmark (CPU path) | none | ~10m | none (no GPU needed) |

**Nightly failure:** Posts to `#code` Discord channel with failing test details. Does not block next day's PRs but must be investigated within 24 hours.

### 11.3 Release Gate (Before Any Production Deploy)

**Runtime:** 4–6 hours. Requires GPU + full staging infrastructure on Akash.

| Test Suite | Tags | Time | GPU? | Dependencies |
|-----------|------|------|------|-------------|
| Scale test S3 (1,000 agents, Kimi K2.5) | `scale-tests, gpu-tests` | ~45m | Yes | Full Akash staging |
| Scale test S4 (1,500 agents, stress) | `scale-tests, gpu-tests` | ~60m | Yes | Full Akash staging |
| PARL test with real Kimi K2.5 | `inference-tests, gpu-tests` | ~30m | Yes | Kimi K2.5 GPU endpoint |
| FHE benchmark (GPU path) | `gpu-tests` | ~20m | Yes | CUDA runtime |
| FHE state operations at 1,000 RESTRICTED agents | `scale-tests, gpu-tests` | ~30m | Yes | Full staging |
| RESTRICTED POD routing overhead benchmark | `gpu-tests, inference-tests` | ~20m | Yes | Tier A Claude proxy |
| Chaos full suite with Litmus (K8s-level) | `chaos-tests` | ~60m | No | K8s + Litmus |
| Dapr sidecar crash at scale (100 agents live) | `chaos-tests` | ~20m | No | K8s + Litmus |
| Ray Serve OOM injection | `chaos-tests, gpu-tests` | ~20m | Yes | GPU K8s |
| Full 24-hour soak test | `scale-tests, gpu-tests` | 24h | Yes | Full Akash staging |

**Release gate failures are hard blocks.** No production deploy until all release gate tests pass.

---

## 12. Prometheus Queries (SLO Observability)

### 12.1 Agent Hydration (Cold Start) SLO

```promql
# P95 cold start latency — must be ≤ 50ms
histogram_quantile(
  0.95,
  sum(rate(swarm_agent_hydration_duration_seconds_bucket[5m]))
    by (le, instance)
)

# P99 cold start latency — must be ≤ 200ms
histogram_quantile(
  0.99,
  sum(rate(swarm_agent_hydration_duration_seconds_bucket[5m]))
    by (le)
)

# Cold start rate — how often are agents being hydrated?
sum(rate(swarm_agents_hydrated_total[1m]))

# Fraction of agents currently frozen (healthy sign if high — scale-to-zero working)
swarm_active_agents_total{state="frozen"}
  /
(swarm_active_agents_total{state="frozen"} + swarm_active_agents_total{state="active"})
```

### 12.2 Message Dispatch Latency SLO

```promql
# L1 (Flume) P99 dispatch latency — must be ≤ 5µs
histogram_quantile(
  0.99,
  sum(rate(swarm_message_dispatch_duration_seconds_bucket{tier="l1"}[5m]))
    by (le)
)

# L3 (NATS) P99 dispatch latency — must be ≤ 200µs (cross-machine)
histogram_quantile(
  0.99,
  sum(rate(swarm_message_dispatch_duration_seconds_bucket{tier="l3"}[5m]))
    by (le)
)

# End-to-end task latency P99 (all tiers)
histogram_quantile(
  0.99,
  sum(rate(swarm_task_e2e_duration_seconds_bucket[5m]))
    by (le, model_tier)
)
```

### 12.3 Inference Cluster Utilization

```promql
# vLLM Kimi K2.5: GPU utilization
vllm_gpu_cache_usage_perc{deployment="kimi-k25"}

# vLLM batch size (should be high for good throughput)
vllm_avg_generation_throughput_toks_per_s{deployment="kimi-k25"}

# Inference request queue depth (high = cluster saturated)
vllm_num_requests_waiting{deployment="kimi-k25"}

# KV cache hit rate (target: >60% for PUBLIC/INTERNAL tasks with shared prefix caching)
rate(vllm_request_prompt_tokens_total{cache_hit="true"}[5m])
  /
rate(vllm_request_prompt_tokens_total[5m])

# Per-agent inference cost attribution (token spend by agent_id label)
sum by (agent_id) (
  increase(swarm_inference_tokens_total{pool="open"}[1h])
)
```

### 12.4 Error Amplification Monitoring

```promql
# Current measured error amplification factor (updated by failure injection tests)
swarm_error_amplification_factor{topology="centralized"}

# Agent quarantine rate (high = something is causing cascade failures)
rate(swarm_agents_quarantined_total[5m])

# Task failure rate by tier
rate(swarm_tasks_completed_total{status="error"}[5m])
  /
rate(swarm_tasks_completed_total[5m])
```

### 12.5 Security / RESTRICTED Routing

```promql
# RESTRICTED task rejection count (pod unavailable — should be 0 in steady state)
increase(swarm_restricted_task_rejections_total[1h])

# RESTRICTED vs OPEN routing split
sum(rate(swarm_tasks_dispatched_total{tier="restricted"}[5m]))
sum(rate(swarm_tasks_dispatched_total{tier!="restricted"}[5m]))

# FHE operation latency (RESTRICTED agents)
histogram_quantile(
  0.99,
  sum(rate(swarm_fhe_operation_duration_seconds_bucket[5m]))
    by (le, op_type)
)

# Multi-party attestation quorum health (should be 1.0 = all verifiers healthy)
swarm_attestation_quorum_healthy_ratio
```

### 12.6 Chaos Test Observability

```promql
# Recovery time after Dapr sidecar crash
swarm_chaos_recovery_duration_seconds{failure_type="dapr_sidecar_crash"}

# Active chaos experiment (1 = chaos active, 0 = normal)
swarm_chaos_experiment_active{experiment_type="nats_partition|dapr_crash|oom|agent_panic"}

# Agent count during chaos (should not drop to 0)
min_over_time(swarm_active_agents_total{state="active"}[5m])
```

---

## 13. Grafana Dashboard Spec

### 13.1 Dashboard: Swarm Runtime Overview

```json
{
  "title": "AF Swarm Runtime — Overview",
  "uid": "af-swarm-overview",
  "tags": ["af-swarm", "production"],
  "time": {"from": "now-1h", "to": "now"},
  "refresh": "15s",
  "panels": [
    {
      "title": "Active Agents by State",
      "type": "timeseries",
      "gridPos": {"x": 0, "y": 0, "w": 12, "h": 6},
      "targets": [
        {
          "expr": "swarm_active_agents_total",
          "legendFormat": "{{state}}"
        }
      ],
      "fieldConfig": {
        "defaults": {
          "color": {"mode": "palette-classic"},
          "unit": "short"
        }
      }
    },
    {
      "title": "Agent Hydration P95 / P99 Latency",
      "type": "timeseries",
      "gridPos": {"x": 12, "y": 0, "w": 12, "h": 6},
      "targets": [
        {
          "expr": "histogram_quantile(0.95, sum(rate(swarm_agent_hydration_duration_seconds_bucket[5m])) by (le))",
          "legendFormat": "P95"
        },
        {
          "expr": "histogram_quantile(0.99, sum(rate(swarm_agent_hydration_duration_seconds_bucket[5m])) by (le))",
          "legendFormat": "P99"
        }
      ],
      "fieldConfig": {"defaults": {"unit": "s"}},
      "thresholds": {
        "mode": "absolute",
        "steps": [
          {"color": "green", "value": null},
          {"color": "yellow", "value": 0.05},
          {"color": "red", "value": 0.2}
        ]
      }
    },
    {
      "title": "Task Throughput",
      "type": "stat",
      "gridPos": {"x": 0, "y": 6, "w": 6, "h": 4},
      "targets": [
        {
          "expr": "sum(rate(swarm_tasks_completed_total{status='success'}[1m]))",
          "legendFormat": "tasks/s"
        }
      ],
      "fieldConfig": {"defaults": {"unit": "reqps"}}
    },
    {
      "title": "Task Error Rate",
      "type": "stat",
      "gridPos": {"x": 6, "y": 6, "w": 6, "h": 4},
      "targets": [
        {
          "expr": "sum(rate(swarm_tasks_completed_total{status='error'}[5m])) / sum(rate(swarm_tasks_completed_total[5m]))",
          "legendFormat": "error rate"
        }
      ],
      "fieldConfig": {
        "defaults": {"unit": "percentunit"},
        "thresholds": {
          "mode": "absolute",
          "steps": [
            {"color": "green", "value": null},
            {"color": "yellow", "value": 0.001},
            {"color": "red", "value": 0.005}
          ]
        }
      }
    },
    {
      "title": "E2E Task Latency (P50 / P95 / P99) by Model",
      "type": "timeseries",
      "gridPos": {"x": 0, "y": 10, "w": 24, "h": 8},
      "targets": [
        {
          "expr": "histogram_quantile(0.5, sum(rate(swarm_task_e2e_duration_seconds_bucket[5m])) by (le, model_tier))",
          "legendFormat": "P50 {{model_tier}}"
        },
        {
          "expr": "histogram_quantile(0.95, sum(rate(swarm_task_e2e_duration_seconds_bucket[5m])) by (le, model_tier))",
          "legendFormat": "P95 {{model_tier}}"
        },
        {
          "expr": "histogram_quantile(0.99, sum(rate(swarm_task_e2e_duration_seconds_bucket[5m])) by (le, model_tier))",
          "legendFormat": "P99 {{model_tier}}"
        }
      ],
      "fieldConfig": {"defaults": {"unit": "s"}}
    }
  ]
}
```

### 13.2 Dashboard: Inference Cluster

```json
{
  "title": "AF Swarm — Inference Cluster",
  "uid": "af-inference-cluster",
  "panels": [
    {
      "title": "vLLM GPU Cache Utilization",
      "type": "gauge",
      "targets": [
        {"expr": "vllm_gpu_cache_usage_perc{deployment='kimi-k25'}", "legendFormat": "Kimi K2.5"}
      ],
      "fieldConfig": {
        "defaults": {"unit": "percent", "min": 0, "max": 100},
        "thresholds": {
          "steps": [
            {"color": "green", "value": null},
            {"color": "yellow", "value": 70},
            {"color": "red", "value": 90}
          ]
        }
      }
    },
    {
      "title": "KV Cache Hit Rate (prefix caching efficiency)",
      "type": "timeseries",
      "targets": [
        {
          "expr": "rate(vllm_request_prompt_tokens_total{cache_hit='true'}[5m]) / rate(vllm_request_prompt_tokens_total[5m])",
          "legendFormat": "{{deployment}}"
        }
      ],
      "fieldConfig": {"defaults": {"unit": "percentunit"}}
    },
    {
      "title": "Requests Waiting (queue depth)",
      "type": "timeseries",
      "targets": [
        {"expr": "vllm_num_requests_waiting", "legendFormat": "{{deployment}}"}
      ]
    },
    {
      "title": "Token Spend by Agent (top 20)",
      "type": "table",
      "targets": [
        {
          "expr": "topk(20, sum by (agent_id) (increase(swarm_inference_tokens_total{pool='open'}[1h])))",
          "legendFormat": "{{agent_id}}"
        }
      ]
    },
    {
      "title": "RESTRICTED POD Health",
      "type": "stat",
      "targets": [
        {"expr": "swarm_restricted_pod_healthy", "legendFormat": "Restricted POD"}
      ],
      "fieldConfig": {
        "defaults": {"mappings": [{"type": "value", "options": {"0": {"text": "DOWN", "color": "red"}, "1": {"text": "UP", "color": "green"}}}]}
      }
    }
  ]
}
```

### 13.3 Dashboard: Scale Test Results

```json
{
  "title": "AF Swarm — Scale Test Results",
  "uid": "af-scale-test",
  "panels": [
    {
      "title": "Concurrent Agent Count Over Time",
      "type": "timeseries",
      "targets": [
        {"expr": "swarm_active_agents_total{state='active'}", "legendFormat": "Active"},
        {"expr": "swarm_active_agents_total{state='hydrating'}", "legendFormat": "Hydrating"},
        {"expr": "swarm_active_agents_total{state='frozen'}", "legendFormat": "Frozen"}
      ]
    },
    {
      "title": "Hydration Rate (agents/second)",
      "type": "timeseries",
      "targets": [
        {"expr": "rate(swarm_agents_hydrated_total[30s])", "legendFormat": "hydrations/s"}
      ]
    },
    {
      "title": "Error Amplification Factor (measured)",
      "type": "stat",
      "targets": [
        {"expr": "swarm_error_amplification_factor{topology='centralized'}", "legendFormat": "Centralized"},
        {"expr": "swarm_error_amplification_factor{topology='flat_p2p'}", "legendFormat": "Flat P2P"}
      ]
    }
  ]
}
```

---

## 14. Alert Rule Definitions

```yaml
# ops/prometheus/alerts/af-swarm-slos.yml
groups:
  - name: af_swarm_slos
    interval: 30s
    rules:

      # Cold start SLO breach
      - alert: AgentHydrationP99Exceeded
        expr: |
          histogram_quantile(
            0.99,
            sum(rate(swarm_agent_hydration_duration_seconds_bucket[5m])) by (le)
          ) > 0.5
        for: 2m
        labels:
          severity: warning
          team: platform
        annotations:
          summary: "Agent cold start P99 exceeds 500ms SLO"
          description: "P99 hydration latency is {{ $value | humanizeDuration }}. SLO: ≤500ms."
          runbook: "https://github.com/alternatefutures/admin/blob/main/docs/runbooks/agent-hydration-latency.md"

      - alert: AgentHydrationP99Critical
        expr: |
          histogram_quantile(
            0.99,
            sum(rate(swarm_agent_hydration_duration_seconds_bucket[5m])) by (le)
          ) > 2.0
        for: 1m
        labels:
          severity: critical
          team: platform
        annotations:
          summary: "Agent cold start P99 critically high (>2s)"
          description: "P99 hydration latency is {{ $value | humanizeDuration }}. Possible Dapr state store issue."

      # Task error rate SLO
      - alert: TaskErrorRateHigh
        expr: |
          sum(rate(swarm_tasks_completed_total{status="error"}[5m]))
          /
          sum(rate(swarm_tasks_completed_total[5m]))
          > 0.005
        for: 3m
        labels:
          severity: warning
          team: platform
        annotations:
          summary: "Task error rate exceeds 0.5%"
          description: "Current error rate: {{ $value | humanizePercentage }}."

      # RESTRICTED task rejection (should be 0 — any rejection is an incident)
      - alert: RestrictedTaskRejected
        expr: increase(swarm_restricted_task_rejections_total[5m]) > 0
        for: 0m
        labels:
          severity: critical
          team: security
        annotations:
          summary: "RESTRICTED task rejected — RESTRICTED POD unavailable"
          description: "{{ $value }} RESTRICTED task(s) rejected in last 5 minutes. RESTRICTED POD may be down."
          runbook: "https://github.com/alternatefutures/admin/blob/main/docs/runbooks/restricted-pod-unavailable.md"

      # Agent quarantine rate spike (cascade failure indicator)
      - alert: AgentQuarantineRateHigh
        expr: rate(swarm_agents_quarantined_total[5m]) > 0.1
        for: 2m
        labels:
          severity: warning
          team: platform
        annotations:
          summary: "Agents quarantining at high rate — possible cascade failure"
          description: "{{ $value | humanize }} agents/second being quarantined."

      # Inference cluster queue saturation
      - alert: InferenceQueueSaturated
        expr: vllm_num_requests_waiting{deployment="kimi-k25"} > 500
        for: 3m
        labels:
          severity: warning
          team: platform
        annotations:
          summary: "Kimi K2.5 inference queue > 500 requests waiting"
          description: "Queue depth: {{ $value }}. Consider scaling up vLLM replicas."

      - alert: InferenceQueueCritical
        expr: vllm_num_requests_waiting{deployment="kimi-k25"} > 1000
        for: 1m
        labels:
          severity: critical
          team: platform
        annotations:
          summary: "Kimi K2.5 inference queue critically saturated"
          description: "Queue depth {{ $value }} — tasks will time out. Immediate action required."

      # GPU cache utilization (high = good for performance, but > 95% = risk of OOM)
      - alert: VllmGpuCacheNearFull
        expr: vllm_gpu_cache_usage_perc{deployment="kimi-k25"} > 90
        for: 5m
        labels:
          severity: warning
          team: platform
        annotations:
          summary: "Kimi K2.5 GPU KV cache > 90% full"
          description: "Cache utilization: {{ $value }}%. May cause request failures."

      # FHE operation latency (unexpected spikes indicate load or key management issues)
      - alert: FheOperationLatencyHigh
        expr: |
          histogram_quantile(
            0.99,
            sum(rate(swarm_fhe_operation_duration_seconds_bucket[5m])) by (le, op_type)
          ) > 1.0
        for: 2m
        labels:
          severity: warning
          team: security
        annotations:
          summary: "FHE operation P99 latency > 1s"
          description: "FHE op {{ $labels.op_type }} P99: {{ $value | humanizeDuration }}. GPU may be unavailable."

      # Attestation quorum health
      - alert: AttestationQuorumDegraded
        expr: swarm_attestation_quorum_healthy_ratio < 0.8
        for: 1m
        labels:
          severity: critical
          team: security
        annotations:
          summary: "Multi-party attestation quorum degraded"
          description: "Only {{ $value | humanizePercentage }} of verifiers healthy. RESTRICTED operations at risk."

      # Scale ceiling approaching
      - alert: ActiveAgentCountHigh
        expr: swarm_active_agents_total{state="active"} > 900
        for: 5m
        labels:
          severity: warning
          team: platform
        annotations:
          summary: "Active agent count approaching 1,000 ceiling"
          description: "{{ $value }} agents active. Approaching tested scale limit. Consider capacity review."

  - name: af_swarm_chaos
    rules:
      # Alert if chaos experiment is running in production (should never be)
      - alert: ChaosExperimentInProduction
        expr: swarm_chaos_experiment_active > 0
        for: 0m
        labels:
          severity: critical
          team: platform
          environment: production
        annotations:
          summary: "Chaos experiment is ACTIVE in production"
          description: "Experiment type: {{ $labels.experiment_type }}. This should only run in staging."
```

---

## 15. Open Questions

The following questions cross-cut multiple tracks and cannot be resolved by this document alone. They are surfaced here as required human decisions per RFC §6 convention.

| # | Question | Blocks |
|---|----------|--------|
| **OQ-TB-1** | Dapr cold start SLO: should P95 ≤ 50ms or P99 ≤ 50ms? The RFC says "sub-50ms" without specifying percentile. The RFC open question OQ-5 says "P99 hydration <500ms" as baseline definition. These are not contradictory but both need confirmation. | §4.1 test thresholds |
| **OQ-TB-2** | End-to-end latency SLOs (§4.4) depend on Atlas's (#132) vLLM batch-size configuration. Review the SLO table after PR #132 merges. | §4.4, §14 alert thresholds |
| **OQ-TB-3** | Error amplification measurement depends on our task graph structure. If measured factor differs materially from 4.4×, this must be raised as a review question on RFC §1.4 before the RFC is marked ACCEPTED. The test is designed to measure, not validate, the RFC's exact number. | §5.3 |
| **OQ-TB-4** | PARL test requires orchestrator API to expose raw task DAG (`plan_task_dag(prompt) → TaskDag`). Confirm with Chief Architect (#136) that the runtime→orchestrator interface includes this introspection endpoint. | §6.4 |
| **OQ-TB-5** | Tool-coordination token attribution requires per-step token counts in the inference response. Confirm with Chief Architect (#136) that the inference proto includes this. Fallback: use total token count + latency proxy only. | §7.2 |
| **OQ-TB-6** | Dapr sidecar crash behavior: can the Rust runtime continue processing already-hydrated agents if the sidecar goes down? Depends on Lain's (#131) `DaprActivationListenerActor` design (OQ-RI-1). Review this test after PR #131 merges. | §9.5 |

---

## 16. References

1. RFC 001: 1000-Agent Amendment — [`docs/architecture/reviews/1000-agent-amendment.md`](../reviews/1000-agent-amendment.md) (PR #128)
2. v3.0 Plan — [`docs/architecture/rust-swarm-runtime.md`](../rust-swarm-runtime.md)
3. Runtime Internals Design — [`docs/architecture/design/rust-runtime-internals.md`](./rust-runtime-internals.md) (PR #131, Lain)
4. Inference Cluster Design — [`docs/architecture/design/inference-cluster.md`](./inference-cluster.md) (PR #132, Atlas)
5. Security Multi-Tenant Design — [`docs/architecture/design/security-multi-tenant.md`](./security-multi-tenant.md) (PR #133, Argus)
6. Google/MIT, "Towards a Science of Scaling Agent Systems," Mar 2026 — [research.google/blog/towards-a-science-of-scaling-agent-systems](https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/)
7. Moonshot AI, Kimi K2.5 PARL — [kimi.com/blog/kimi-k2-5](https://www.kimi.com/blog/kimi-k2-5)
8. Toxiproxy — [github.com/Shopify/toxiproxy](https://github.com/Shopify/toxiproxy)
9. LitmusChaos — [litmuschaos.io](https://litmuschaos.io)
10. vLLM — [docs.vllm.ai](https://docs.vllm.ai)
11. Dapr Actors — [diagrid.io/blog/understanding-dapr-actors](https://www.diagrid.io/blog/understanding-dapr-actors-for-scalable-workflows-and-ai-agents)
12. TFHE-rs — [github.com/zama-ai/tfhe-rs](https://github.com/zama-ai/tfhe-rs)
13. MAST Failure Taxonomy (v3.0 §5) + RFC additions (FM-19, FM-20)

---

*Authored by QA Engineer, 2026-04-16. Cross-track review required from Lain (#131), Atlas (#132), and Argus (#133) before test infrastructure implementation begins. Open questions (§15) require human or cross-track resolution.*
