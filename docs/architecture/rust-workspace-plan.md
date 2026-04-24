# AF Swarm Runtime — Rust Workspace Plan

**Version:** 1.0
**Date:** 2026-02-15
**Status:** Implementation blueprint
**Based on:** [rust-swarm-runtime.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/rust-swarm-runtime.md), [chief-architect-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md), [depin-tee-architecture.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/revisions/depin-tee-architecture.md), [senku-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md), [senku-memory-deep-dive.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-memory-deep-dive.md), [qa-engineer-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md), [lain-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/lain-review.md)

**Purpose:** This is the document someone would use to `cargo init` the project. It defines every crate, every dependency, every trait boundary, and the exact build order.

---

## Table of Contents

1. [Cargo Workspace Layout](#1-cargo-workspace-layout)
2. [Crate Specifications](#2-crate-specifications)
3. [Dependency Graph](#3-dependency-graph)
4. [Build Configuration](#4-build-configuration)
5. [Migration Bridge](#5-migration-bridge)
6. [Revised 14-Phase Build Plan](#6-revised-14-phase-build-plan)

---

## 1. Cargo Workspace Layout

### 1.1 Directory Structure

```
af-swarm/
├── Cargo.toml                    # Workspace root
├── Cargo.lock
├── rust-toolchain.toml           # Pin nightly (for async trait in stable, miri)
├── deny.toml                     # cargo-deny config (license + advisory audit)
├── clippy.toml                   # Workspace-wide clippy config
├── .cargo/
│   └── config.toml               # Build profiles, linker config
├── proto/
│   ├── agent.proto               # Agent messages, task lifecycle
│   ├── memory.proto              # Memory vectors, retrieval
│   ├── transport.proto           # NATS bridge wire format
│   └── a2a.proto                 # A2A protocol messages
├── crates/
│   ├── af-core/                  # Shared types, error types, config
│   ├── af-actors/                # Ractor-based actor definitions
│   ├── af-models/                # Rig abstraction layer
│   ├── af-memory/                # LanceDB + SYNAPSE + MemAgent
│   ├── af-transport/             # NATS client, Flume channels
│   ├── af-sandbox/               # Wasmtime WASM + bubblewrap fallback
│   ├── af-security/              # DSE classifier, TLS, FHE, ZK
│   ├── af-a2a/                   # A2A gateway, Agent Cards
│   ├── af-orchestrator/          # PARL-lite, task routing, scheduling
│   ├── af-cli/                   # Commander-style CLI
│   └── af-server/                # axum HTTP + tonic gRPC
├── bridges/
│   └── af-napi/                  # napi-rs bridge for TS interop
├── tools/
│   ├── af-migrate/               # TS memory → LanceDB migration tool
│   └── af-bench/                 # Benchmarking harness
├── tests/
│   ├── integration/              # Cross-crate integration tests
│   └── fixtures/                 # Test data, mock configs
└── docs/
    └── adr/                      # Architecture Decision Records
```

### 1.2 Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/af-core",
    "crates/af-actors",
    "crates/af-models",
    "crates/af-memory",
    "crates/af-transport",
    "crates/af-sandbox",
    "crates/af-security",
    "crates/af-a2a",
    "crates/af-orchestrator",
    "crates/af-cli",
    "crates/af-server",
    "bridges/af-napi",
    "tools/af-migrate",
    "tools/af-bench",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"
repository = "https://github.com/alternatefutures/af-swarm"
rust-version = "1.85"

[workspace.dependencies]
# Async runtime
tokio = { version = "1.43", features = ["full"] }
tokio-util = "0.7"
tokio-stream = "0.1"
futures = "0.3"
async-trait = "0.1"

# Actor framework
ractor = "0.15"

# LLM client (pinned — pre-1.0 API instability)
rig-core = "=0.30"

# Channels (replacing kanal per chief architect review)
flume = "0.11"

# Serialization — split strategy per senku review
prost = "0.13"                     # NATS wire format, persistence
prost-types = "0.13"
serde = { version = "1", features = ["derive"] }
serde_json = "1"                   # Debug/logging
toml = "0.8"                       # Agent config files

# Vector store
lancedb = "0.23"
lance = "0.23"

# Embedding
fastembed = "4"                    # ONNX-based, <1ms/doc

# Local inference
mistralrs = "0.5"                  # Qwen2.5-0.5B for routing

# NATS
async-nats = "0.39"

# HTTP server
axum = { version = "0.8", features = ["ws", "macros"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace", "compression-gzip"] }

# gRPC
tonic = "0.12"
tonic-build = "0.12"

# HTTP client
reqwest = { version = "0.12", features = ["json", "stream", "rustls-tls"] }

# Sandbox
wasmtime = "28"
wasmtime-wasi = "28"

# Security
ring = "0.17"                      # Ed25519, HMAC, SHA-256
rustls = "0.23"                    # TLS

# Config hot-reload
arc-swap = "1"
notify = "7"

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-opentelemetry = "0.28"
opentelemetry = "0.27"
opentelemetry-otlp = "0.27"
prometheus = "0.13"

# Utilities
thiserror = "2"
anyhow = "1"
uuid = { version = "1", features = ["v7"] }
chrono = { version = "0.4", features = ["serde"] }
bytes = "1"
dashmap = "6"
regex = "1"
clap = { version = "4", features = ["derive"] }

# Testing
criterion = { version = "0.5", features = ["async_tokio"] }
wiremock = "0.6"
testcontainers = "0.23"
insta = "1"                        # Snapshot testing
proptest = "1"                     # Property-based testing

[workspace.lints.rust]
unsafe_code = "deny"

[workspace.lints.clippy]
all = "warn"
pedantic = "warn"
nursery = "warn"
unwrap_used = "deny"
expect_used = "warn"
```

---

## 2. Crate Specifications

### 2.1 af-core

**Purpose:** Shared types, error taxonomy, configuration schema, and foundational traits used by every other crate.

**Public API Surface:**

```rust
// === Error Types ===
#[derive(Debug, thiserror::Error)]
pub enum AfError {
    #[error("agent {id} not found")]
    AgentNotFound { id: AgentId },
    #[error("model provider {provider} unavailable: {reason}")]
    ModelUnavailable { provider: String, reason: String },
    #[error("budget exceeded for agent {agent}: {detail}")]
    BudgetExceeded { agent: AgentId, detail: String },
    #[error("sandbox violation: {0}")]
    SandboxViolation(String),
    #[error("memory error: {0}")]
    Memory(#[from] MemoryError),
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("security: {0}")]
    Security(#[from] SecurityError),
    #[error("config: {0}")]
    Config(#[from] ConfigError),
    // ... additional variants
}

pub type AfResult<T> = Result<T, AfError>;

// === Agent Identity ===
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: AgentId,
    pub name: String,
    pub role: String,
    pub personality: PersonalityConfig,
    pub model: ModelRouting,
    pub capabilities: CapabilitySet,
    pub budget: BudgetConfig,
    pub memory: MemoryConfig,
    pub tools: Vec<ToolPermission>,
    pub version: semver::Version,
}

// === Messages ===
#[derive(Debug, Clone)]
pub enum AgentMessage {
    Task(TaskEnvelope),
    Response(TaskResponse),
    Reconfigure(AgentConfig),        // Hot-reload (ArcSwap pattern)
    Heartbeat,
    Shutdown { graceful: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEnvelope {
    pub id: TaskId,
    pub correlation_id: CorrelationId,  // Distributed tracing
    pub source: AgentId,
    pub target: AgentId,
    pub payload: TaskPayload,
    pub priority: Priority,
    pub budget: TaskBudget,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
}

// === Configuration ===
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub agents_dir: PathBuf,
    pub nats: NatsConfig,
    pub memory: GlobalMemoryConfig,
    pub security: SecurityConfig,
    pub observability: ObservabilityConfig,
    pub server: ServerConfig,
    pub hot_reload: HotReloadConfig,
}

// === Capability Model ===
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub filesystem: FsCapability,      // None | ReadOnly(paths) | ReadWrite(paths)
    pub network: NetworkCapability,    // None | AllowList(domains) | Unrestricted
    pub shell: ShellCapability,        // None | Sandboxed | Unrestricted
    pub spawn_agents: bool,
    pub max_concurrent_tools: u32,
}
```

**Dependencies (internal):** None — leaf crate.

**Dependencies (external):** `thiserror`, `anyhow`, `serde`, `serde_json`, `toml`, `chrono`, `uuid`, `semver`, `tracing`, `bytes`, `prost` (for proto-generated types)

**Feature flags:**
- `proto` (default) — Include prost-generated message types
- `test-fixtures` — Expose test helpers and mock configs

**Estimated LoC:** ~2,500

---

### 2.2 af-actors

**Purpose:** Ractor-based actor definitions for all agent types. Supervision tree setup, actor lifecycle, mailbox monitoring, and the agent runtime loop.

**Public API Surface:**

```rust
// === Agent Actor ===
pub struct AgentActor {
    config: Arc<ArcSwap<AgentConfig>>,  // Hot-reloadable
    model: Arc<dyn ModelProvider>,
    memory: Arc<dyn MemoryEngine>,
    tools: Arc<ToolRegistry>,
    metrics: AgentMetrics,
}

impl ractor::Actor for AgentActor {
    type Msg = AgentMessage;
    type State = AgentState;
    type Arguments = AgentActorArgs;
    // ...
}

// === Supervision Tree ===
pub struct SwarmSupervisor;

impl SwarmSupervisor {
    pub async fn start(config: RuntimeConfig) -> AfResult<ActorRef<SwarmSupervisorMsg>>;
}

pub struct GroupSupervisor {
    pub group: AgentGroup,  // Marketing, Code, Infrastructure, Dynamic
    pub restart_strategy: RestartStrategy,
}

// === Agent State ===
pub struct AgentState {
    pub working_memory: MemAgentController,
    pub active_tasks: HashMap<TaskId, TaskState>,
    pub mailbox_depth: AtomicU64,       // Backpressure monitoring
    pub consecutive_failures: u32,
    pub last_heartbeat: Instant,
}

// === Actor Registry ===
pub fn register_agent(id: &AgentId, actor_ref: ActorRef<AgentMessage>);
pub fn lookup_agent(id: &AgentId) -> Option<ActorRef<AgentMessage>>;
pub fn list_agents() -> Vec<(AgentId, ActorRef<AgentMessage>)>;

// === Backpressure ===
/// Semaphore-based rate limiter wrapping Ractor's unbounded mailbox.
/// If mailbox exceeds `max_depth`, the sender gets backpressure.
pub struct BoundedMailboxWrapper<M> {
    inner: ActorRef<M>,
    semaphore: Arc<Semaphore>,
    max_depth: usize,
}
```

**Dependencies (internal):** `af-core`, `af-models`, `af-memory`, `af-transport`, `af-sandbox`

**Dependencies (external):** `ractor`, `tokio`, `arc-swap`, `tracing`, `dashmap`

**Feature flags:**
- `distributed` — Enable `ractor_cluster` for multi-node (experimental, not for v1)
- `mock-actors` — Test doubles for integration testing

**Estimated LoC:** ~3,500

---

### 2.3 af-models

**Purpose:** Rig abstraction layer with graceful degradation. Wraps Rig behind a stable `ModelProvider` trait so the crate can be swapped without rewriting agent logic. Includes SLM/LLM router and failover chain.

**Public API Surface:**

```rust
// === Core Trait (wraps Rig) ===
#[async_trait]
pub trait ModelProvider: Send + Sync + 'static {
    async fn generate(&self, req: GenerateRequest) -> AfResult<GenerateResponse>;
    async fn stream(&self, req: GenerateRequest)
        -> AfResult<Pin<Box<dyn Stream<Item = AfResult<StreamChunk>> + Send>>>;
    fn supports_tools(&self) -> bool;
    fn model_id(&self) -> &str;
    fn provider_name(&self) -> &str;
}

// === Provider Implementations ===
pub struct AnthropicProvider { /* Rig Anthropic client */ }
pub struct OpenAiProvider { /* Rig OpenAI client — also Ollama, vLLM, Groq */ }
pub struct GeminiProvider { /* Rig Google client */ }
pub struct LocalProvider { /* mistral.rs FFI for SLM tasks */ }
pub struct FallbackProvider {
    primary: Box<dyn ModelProvider>,
    fallback: Box<dyn ModelProvider>,
    slm_fallback: Option<Box<dyn ModelProvider>>,
}

// === SLM/LLM Router ===
pub struct TaskRouter {
    classifier: LocalProvider,  // Qwen2.5-0.5B for complexity classification
    cloud_provider: Box<dyn ModelProvider>,
    local_provider: Box<dyn ModelProvider>,
    threshold: f32,  // complexity score below this → SLM
}

impl TaskRouter {
    pub async fn route(&self, task: &TaskEnvelope) -> AfResult<Box<dyn ModelProvider>>;
}

// === Model Failover Chain ===
/// Failover: primary → fallback → SLM → queue-and-retry → reject
pub struct FailoverChain {
    providers: Vec<Box<dyn ModelProvider>>,
    retry_config: RetryConfig,
    circuit_breaker: CircuitBreaker,
}

// === Graceful Degradation ===
pub struct CircuitBreaker {
    failure_threshold: u32,
    reset_timeout: Duration,
    half_open_max: u32,
    state: AtomicU8,  // Closed=0, Open=1, HalfOpen=2
}

// === Embedding Engine ===
/// fastembed-rs (ort) for vector embeddings — separate from text generation
pub struct EmbeddingEngine {
    model: fastembed::TextEmbedding,
}

impl EmbeddingEngine {
    pub fn embed(&self, text: &str) -> AfResult<Vec<f32>>;
    pub fn embed_batch(&self, texts: &[&str]) -> AfResult<Vec<Vec<f32>>>;
}
```

**Dependencies (internal):** `af-core`

**Dependencies (external):** `rig-core` (pinned), `mistralrs`, `fastembed`, `reqwest`, `tokio`, `tracing`, `arc-swap`

**Feature flags:**
- `anthropic` (default) — Anthropic provider via Rig
- `openai` (default) — OpenAI-compatible provider via Rig
- `gemini` — Google Gemini provider via Rig
- `local-inference` — mistral.rs for local SLM (adds ~50MB binary size)
- `mock-provider` — Canned responses for testing (returns deterministic output)

**Estimated LoC:** ~3,000

---

### 2.4 af-memory

**Purpose:** 3-subsystem memory engine: LanceDB (persistent vectors) + SYNAPSE (graph retrieval) + MemAgent (O(1) working memory). Includes VectorStore trait for backend swappability, compaction job, and migration from TS 5-layer system.

**Public API Surface:**

```rust
// === VectorStore Trait (from senku review — enables LanceDB → S3 → pgvector migration) ===
#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn insert(&self, id: &str, vector: &[f32], metadata: serde_json::Value) -> AfResult<()>;
    async fn search(&self, query: &[f32], k: usize, filter: Option<Filter>) -> AfResult<Vec<SearchResult>>;
    async fn delete(&self, id: &str) -> AfResult<()>;
    async fn compact(&self) -> AfResult<CompactionStats>;
    async fn count(&self) -> AfResult<usize>;
}

pub struct LanceVectorStore { /* embedded LanceDB */ }

// === SYNAPSE Graph Memory ===
pub struct SynapseGraph {
    nodes: HashMap<String, SynapseNode>,
    edges: Vec<SynapseEdge>,
}

impl SynapseGraph {
    /// Spreading activation retrieval — replaces naive cosine similarity (+23% accuracy)
    pub fn retrieve(
        &self,
        query_embedding: &[f32],
        store: &dyn VectorStore,
        k: usize,
        config: &SynapseConfig,
    ) -> AfResult<Vec<MemoryEntry>>;

    pub fn add_node(&mut self, memory_id: &str, embedding: &[f32]);
    pub fn add_edge(&mut self, from: &str, to: &str, weight: f32);
}

pub struct SynapseConfig {
    pub decay_factor: f32,       // Within-query activation decay (NOT temporal decay)
    pub spread_factor: f32,      // How much activation propagates per edge
    pub max_iterations: u32,     // Spreading activation iterations
    pub initial_candidates: usize, // Top-N from ANN search before spreading
}

// === MemAgent Controller (O(1) working memory) ===
pub struct MemAgentController {
    buffer: BoundedBuffer<MemorySlot>,
    config: MemAgentConfig,
}

pub struct MemAgentConfig {
    pub buffer_size: usize,              // Default: 32 slots
    pub eviction_policy: EvictionPolicy,
    pub summarize_on_evict: bool,        // Absorbs SimpleMem's role
    pub summary_max_tokens: usize,
    pub scoring_weights: ScoringWeights,
}

pub struct ScoringWeights {
    pub recency: f32,    // w_r (default: 0.4)
    pub relevance: f32,  // w_s (default: 0.4)
    pub frequency: f32,  // w_a (default: 0.2)
}

pub enum EvictionPolicy {
    LruRelevance,   // Combines recency + relevance score (default)
    PureRelevance,  // Evict lowest relevance regardless of recency
    PureLru,        // Evict least recently used
}

impl MemAgentController {
    pub fn query(&mut self, input_embedding: &[f32]) -> Vec<&MemorySlot>;
    pub fn insert(&mut self, entry: MemoryEntry) -> Option<MemoryEntry>; // Returns evicted entry
    pub fn pre_warm(&mut self, entries: Vec<MemoryEntry>);
    pub fn buffer_snapshot(&self) -> Vec<MemorySlot>;
}

// === Compaction Job (absorbs FadeMem's role) ===
pub struct CompactionConfig {
    pub interval_hours: u32,              // Default: 6
    pub ttl_days: u32,                    // Default: 90
    pub immortal_access_threshold: u32,   // Default: 10
    pub min_relevance_score: f32,         // Default: 0.1
}

pub struct CompactionJob {
    store: Arc<dyn VectorStore>,
    graph: Arc<RwLock<SynapseGraph>>,
    config: CompactionConfig,
}

impl CompactionJob {
    pub async fn run_once(&self) -> AfResult<CompactionStats>;
    pub fn spawn_periodic(self, handle: &tokio::runtime::Handle) -> JoinHandle<()>;
}

// === Unified Memory Engine ===
pub struct MemoryEngine {
    store: Arc<dyn VectorStore>,
    graph: Arc<RwLock<SynapseGraph>>,
    embedder: Arc<EmbeddingEngine>,
    compaction: CompactionJob,
}

impl MemoryEngine {
    /// Full retrieval pipeline: MemAgent buffer → SYNAPSE → LanceDB
    pub async fn retrieve(
        &self,
        agent_id: &AgentId,
        query: &str,
        memagent: &mut MemAgentController,
        k: usize,
    ) -> AfResult<Vec<MemoryEntry>>;

    /// Store a new memory
    pub async fn store(
        &self,
        agent_id: &AgentId,
        content: &str,
        metadata: MemoryMetadata,
        memagent: &mut MemAgentController,
    ) -> AfResult<()>;
}
```

**Dependencies (internal):** `af-core`, `af-models` (for `EmbeddingEngine`)

**Dependencies (external):** `lancedb`, `lance`, `fastembed`, `tokio`, `dashmap`, `tracing`, `chrono`

**Feature flags:**
- `lance-embedded` (default) — Local LanceDB storage
- `lance-s3` — S3-compatible remote storage (Phase 2 horizontal scaling)
- `pgvector` — PostgreSQL pgvector backend (Phase 3)

**Estimated LoC:** ~4,500

---

### 2.5 af-transport

**Purpose:** Tiered messaging — Flume for in-process L1, tokio::broadcast for L2 fan-out, async-nats for L3 cross-machine. Message router that auto-detects local vs remote targets. NATS bridge with prost serialization.

**Public API Surface:**

```rust
// === Transport Trait (from senku review — swappable backend) ===
#[async_trait]
pub trait RemoteTransport: Send + Sync {
    async fn publish(&self, subject: &str, payload: &[u8]) -> AfResult<()>;
    async fn subscribe(&self, subject: &str) -> AfResult<MessageStream>;
    async fn request(&self, subject: &str, payload: &[u8], timeout: Duration) -> AfResult<TransportMessage>;
    async fn queue_subscribe(&self, subject: &str, queue: &str) -> AfResult<MessageStream>;
}

pub struct NatsTransport { /* async-nats client */ }

// === Message Router ===
pub struct MessageRouter {
    local_channels: DashMap<AgentId, flume::Sender<AgentMessage>>,
    broadcast: tokio::sync::broadcast::Sender<BroadcastMessage>,
    remote: Arc<dyn RemoteTransport>,
}

impl MessageRouter {
    /// Routes to local Flume channel if target is in-process,
    /// otherwise serializes with prost and sends via NATS.
    pub async fn send(&self, target: &AgentId, msg: AgentMessage) -> AfResult<()>;
    pub async fn broadcast(&self, msg: BroadcastMessage) -> AfResult<()>;
    pub fn register_local(&self, id: AgentId) -> flume::Receiver<AgentMessage>;
    pub fn deregister_local(&self, id: &AgentId);
}

// === NATS Bridge ===
pub struct NatsBridge {
    client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
    subject_prefix: String,  // "af.v1" per lain review
}

impl NatsBridge {
    pub async fn connect(config: &NatsConfig) -> AfResult<Self>;
    /// Subscribe to remote agent messages, forward to local router
    pub async fn start_inbound(
        &self,
        router: Arc<MessageRouter>,
    ) -> AfResult<JoinHandle<()>>;
}

// === Subject Hierarchy (from lain review) ===
// af.v1.agents.{agent_id}.tasks     — Task messages
// af.v1.agents.{agent_id}.responses — Task responses
// af.v1.broadcast.{topic}           — Fan-out topics
// af.v1.system.heartbeat            — Health pings
// af.v1.system.config               — Config update notifications
```

**Dependencies (internal):** `af-core`

**Dependencies (external):** `flume`, `async-nats`, `tokio`, `dashmap`, `prost`, `tracing`, `bytes`

**Feature flags:**
- `nats` (default) — NATS remote transport
- `zenoh` — Zenoh transport (future, if NATS latency becomes a bottleneck)

**Estimated LoC:** ~2,000

---

### 2.6 af-sandbox

**Purpose:** Two-tier tool sandboxing. Tier 1: Wasmtime WASM for portable, fast isolation (70-80% of tools). Tier 2: bubblewrap/seccomp fallback for tools requiring native process spawning (shell, git).

**Public API Surface:**

```rust
// === Sandbox Runtime Trait (from depin-tee-architecture.md) ===
#[async_trait]
pub trait SandboxRuntime: Send + Sync {
    async fn create(&self, config: SandboxConfig) -> AfResult<SandboxHandle>;
    async fn exec(&self, handle: &SandboxHandle, cmd: SandboxCommand) -> AfResult<ExecResult>;
    async fn destroy(&self, handle: SandboxHandle) -> AfResult<()>;
    fn is_available(&self) -> bool;
    fn startup_latency(&self) -> Duration;
}

// === WASM Sandbox (Tier 1 — primary, all platforms) ===
pub struct WasmSandbox {
    engine: wasmtime::Engine,
    linker: wasmtime::Linker<WasiCtx>,
    pool: PoolingAllocator,       // <5us instantiation
}

pub struct WasiCapabilities {
    pub filesystem: Vec<PreopenedDir>,  // Scoped read/read-write dirs
    pub network: NetworkPolicy,          // Domain allowlist
    pub cpu_deadline: Duration,          // Epoch-based interruption
    pub memory_limit: usize,             // Max linear memory in bytes
}

impl WasmSandbox {
    pub fn new(config: WasmSandboxConfig) -> AfResult<Self>;
    /// Execute a WASM tool module with the given capabilities
    pub async fn execute_tool(
        &self,
        wasm_module: &[u8],
        input: &[u8],
        capabilities: WasiCapabilities,
    ) -> AfResult<Vec<u8>>;
}

// === Native Sandbox (Tier 2 — Linux only, runtime-detected) ===
pub struct NativeSandbox {
    strategy: NativeStrategy,
}

pub enum NativeStrategy {
    Bubblewrap,  // PID/mount/network namespaces (requires unprivileged userns)
    Seccomp,     // BPF syscall whitelist only (weaker but always works)
}

impl NativeSandbox {
    /// Auto-detect: try unshare(CLONE_NEWUSER), fallback to seccomp
    pub fn detect() -> Self;
}

// === Tool Registry ===
pub struct ToolRegistry {
    wasm_sandbox: WasmSandbox,
    native_sandbox: NativeSandbox,
    tools: HashMap<String, ToolDef>,
}

pub struct ToolDef {
    pub name: String,
    pub sandbox_tier: SandboxTier,  // Wasm or Native
    pub capabilities: CapabilitySet,
    pub timeout: Duration,
}

pub enum SandboxTier {
    Wasm,     // Filesystem, HTTP fetch, JSON processing, MCP client
    Native,   // Shell executor, Git
}

impl ToolRegistry {
    pub async fn execute(&self, tool: &str, input: ToolInput) -> AfResult<ToolOutput>;
}
```

**Dependencies (internal):** `af-core`

**Dependencies (external):** `wasmtime`, `wasmtime-wasi`, `tokio`, `tracing`, `nix` (Linux syscall wrappers)

**Feature flags:**
- `wasm` (default) — Wasmtime sandbox
- `native-sandbox` (default on Linux) — bubblewrap/seccomp
- `firecracker` — Firecracker microVM (requires KVM, Phase 6+ per DePIN revision)

**Estimated LoC:** ~3,000

---

### 2.7 af-security

**Purpose:** Security guardrails — DSE classifier for TEE/sandbox routing, capability enforcement, prompt injection defense, secret management integration, agent identity, resource governor, audit trail.

**Public API Surface:**

```rust
// === Security Classifier (from depin-tee-architecture.md) ===
pub struct SecurityClassifier {
    config: ClassifierConfig,
    pattern_matcher: regex::RegexSet,
    entropy_detector: EntropyCalculator,
}

pub struct TierDecision {
    pub tier: ExecutionTier,
    pub reason: String,
    pub confidence: f64,
    pub matched_rules: Vec<String>,
}

pub enum ExecutionTier {
    Tee(TeeType),
    Sandbox,
}

impl SecurityClassifier {
    pub fn classify(&self, task: &TaskEnvelope, agent: &AgentConfig) -> TierDecision;
}

// === Capability Enforcement ===
pub struct CapabilityChecker;

impl CapabilityChecker {
    /// Verify agent has permission for the requested operation.
    /// Children can only narrow parent capabilities, never widen.
    pub fn check(
        agent: &AgentConfig,
        operation: &Operation,
    ) -> AfResult<()>;
}

// === Prompt Injection Defense (S3) ===
pub struct InjectionDefense {
    canary_tokens: Vec<String>,
    trust_tagger: TrustTagger,
    output_validator: OutputValidator,
}

impl InjectionDefense {
    /// Tag tool output with trust level
    pub fn tag_output(&self, output: &str, source: TrustSource) -> TaggedOutput;
    /// Scan output for injection indicators
    pub fn validate_output(&self, output: &str) -> ValidationResult;
    /// Check if canary tokens leaked (agent compromised → kill)
    pub fn check_canary(&self, output: &str) -> bool;
}

// === Resource Governor (S6) ===
pub struct ResourceGovernor {
    budgets: DashMap<AgentId, AgentBudget>,
    global_circuit_breaker: CircuitBreaker,  // Daily spend limit
}

pub struct AgentBudget {
    pub tokens_per_task: u64,
    pub tokens_per_day: u64,
    pub cost_per_day_usd: f64,
    pub max_concurrent_tools: u32,
    pub max_sub_agents: u32,
    pub used_today: AtomicU64,
    pub cost_today: AtomicU64,
}

impl ResourceGovernor {
    pub fn check_budget(&self, agent: &AgentId, estimated_tokens: u64) -> AfResult<()>;
    pub fn record_usage(&self, agent: &AgentId, tokens: u64, cost_usd: f64);
    /// Circuit breaker: if daily spend exceeds threshold, pause non-critical agents
    pub fn check_global_limit(&self) -> AfResult<()>;
}

// === Agent Identity (S5) ===
pub struct AgentIdentity {
    keypair: ring::signature::Ed25519KeyPair,
    nkey: String,  // NATS NKey format
}

impl AgentIdentity {
    pub fn sign(&self, message: &[u8]) -> Vec<u8>;
    pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool;
}

// === Audit Trail (S7) ===
pub struct AuditLog {
    writer: Box<dyn AuditWriter>,
}

pub struct AuditEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub agent_id: AgentId,
    pub action: AuditAction,
    pub hash: [u8; 32],       // SHA-256 hash chain
    pub prev_hash: [u8; 32],
}

#[async_trait]
pub trait AuditWriter: Send + Sync {
    async fn write(&self, entry: AuditEntry) -> AfResult<()>;
    async fn verify_chain(&self, from: usize, to: usize) -> AfResult<bool>;
}
```

**Dependencies (internal):** `af-core`

**Dependencies (external):** `ring`, `rustls`, `regex`, `dashmap`, `tracing`, `chrono`

**Feature flags:**
- `tee-classifier` (default) — Security classifier for TEE routing
- `fhe-primitives` — Fully Homomorphic Encryption (Phase 12+, experimental)
- `zk-verifier` — Zero-knowledge proof verification (Phase 12+, experimental)

**Estimated LoC:** ~3,500

---

### 2.8 af-a2a

**Purpose:** A2A protocol gateway — Agent Card generation, task lifecycle (send, get, cancel), SSE streaming, webhook push notifications. Built as a pluggable adapter, not a core dependency.

**Public API Surface:**

```rust
// === Agent Card ===
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub capabilities: A2aCapabilities,
    pub skills: Vec<A2aSkill>,
    pub authentication: A2aAuth,
    pub version: String,  // A2A spec version, currently "0.3"
}

impl AgentCard {
    /// Auto-generate from internal AgentConfig
    pub fn from_agent_config(config: &AgentConfig, base_url: &str) -> Self;
}

// === Task Lifecycle ===
pub struct A2aGateway {
    router: Arc<MessageRouter>,
    agent_configs: Arc<ArcSwap<Vec<AgentConfig>>>,
    rate_limiter: RateLimiter,
}

impl A2aGateway {
    /// POST /a2a/tasks/send
    pub async fn submit_task(&self, req: A2aTaskRequest) -> AfResult<A2aTaskResponse>;
    /// GET /a2a/tasks/{id} (SSE stream)
    pub async fn stream_task(&self, task_id: TaskId) -> AfResult<impl Stream<Item = A2aEvent>>;
    /// POST /a2a/tasks/{id}/cancel
    pub async fn cancel_task(&self, task_id: TaskId) -> AfResult<()>;
    /// GET /.well-known/agent.json
    pub fn agent_card(&self) -> AgentCard;
}

// === Rate Limiting (from lain review — 4 layers) ===
pub struct RateLimiter {
    pub ip_limit: RateLimit,       // Per IP
    pub key_limit: RateLimit,      // Per API key
    pub agent_limit: RateLimit,    // Per target agent
    pub global_limit: RateLimit,   // System-wide
}

// === Webhook Push Notifications ===
pub struct WebhookManager {
    subscriptions: DashMap<TaskId, WebhookSubscription>,
}
```

**Dependencies (internal):** `af-core`, `af-transport`

**Dependencies (external):** `axum`, `tokio`, `serde`, `serde_json`, `tracing`, `dashmap`, `tower`

**Feature flags:**
- `a2a-v03` (default) — A2A spec v0.3
- `webhook` — Push notification support (CRUD management)

**Estimated LoC:** ~2,000

---

### 2.9 af-orchestrator

**Purpose:** Task decomposition, scheduling, and failure handling. PARL-lite architecture: DAG planner, width-first scheduler, critical path optimizer, failure detector. Includes DePIN provider adapter layer.

**Public API Surface:**

```rust
// === DAG Task Planner ===
pub struct DagPlanner;

impl DagPlanner {
    /// Parse a complex task into a dependency graph of sub-tasks
    pub async fn plan(
        &self,
        task: &TaskEnvelope,
        model: &dyn ModelProvider,
    ) -> AfResult<TaskDag>;
}

pub struct TaskDag {
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,  // dependency edges
}

pub struct DagNode {
    pub task: TaskEnvelope,
    pub assigned_agent: Option<AgentId>,
    pub estimated_cost: f64,
}

// === Width-First Scheduler (W&D paper) ===
pub struct WidthFirstScheduler {
    max_parallel: usize,
    agent_registry: Arc<dyn Fn(&AgentId) -> Option<ActorRef<AgentMessage>>>,
}

impl WidthFirstScheduler {
    /// Execute a DAG, maximizing parallel branch execution.
    /// Width > depth per W&D research.
    pub async fn execute(&self, dag: &TaskDag) -> AfResult<TaskResult>;
}

// === DePIN Provider Adapters (from depin-tee-architecture.md) ===
#[async_trait]
pub trait DePinProvider: Send + Sync {
    async fn deploy(&self, spec: DeploySpec) -> AfResult<Deployment>;
    async fn teardown(&self, id: DeploymentId) -> AfResult<()>;
    async fn status(&self, id: DeploymentId) -> AfResult<DeploymentStatus>;
    async fn logs(&self, id: DeploymentId) -> AfResult<LogStream>;
    fn supports_tee(&self) -> TeeCapability;
    fn pricing(&self) -> PricingModel;
}

pub struct AkashAdapter { /* SDL + chain-sdk */ }
pub struct PhalaAdapter { /* pRuntime + dStack */ }
// io.net and Aethir adapters added in later phases

pub struct DeploySpec {
    pub image: ContainerImage,
    pub resources: ResourceRequirements,
    pub tee_requirement: TeeRequirement,
    pub network: NetworkPolicy,
    pub provider_preference: Vec<ProviderId>,
    pub max_cost_per_hour: f64,
}

pub enum TeeRequirement {
    None,
    Preferred(TeeType),
    Required(TeeType),
}

// === Provider Selection ===
pub async fn select_provider(
    spec: &DeploySpec,
    providers: &[Box<dyn DePinProvider>],
) -> AfResult<Box<dyn DePinProvider>>;

// === Failure Detection (MAST taxonomy) ===
pub struct FailureDetector {
    circuit_breakers: DashMap<AgentId, CircuitBreaker>,
    failure_taxonomy: MastTaxonomy,  // 14 failure modes
}
```

**Dependencies (internal):** `af-core`, `af-actors`, `af-security`

**Dependencies (external):** `tokio`, `petgraph` (DAG operations), `tracing`, `dashmap`

**Feature flags:**
- `dag-planner` (default) — Basic DAG decomposition
- `width-first` (default) — W&D width-first scheduling
- `lamas-cpo` — LAMaS critical path optimizer (Phase 13, advanced)
- `depin-akash` (default) — Akash provider adapter
- `depin-phala` — Phala TEE provider adapter
- `depin-ionet` — io.net GPU provider adapter
- `depin-aethir` — Aethir GPU provider adapter

**Estimated LoC:** ~4,000

---

### 2.10 af-cli

**Purpose:** Command-line interface for the swarm runtime. Agent management, deployment, inspection, memory debugging, migration tooling.

**Public API Surface:**

```
af-swarm
├── dev                           # Local development
│   ├── start                     # Start runtime locally
│   ├── agent <name>              # Run a single agent in isolation
│   └── repl                      # Interactive message sender (agent REPL)
├── agents
│   ├── list                      # List all registered agents
│   ├── status <name>             # Show agent state, mailbox depth, budget
│   ├── config <name>             # Show/validate agent config
│   ├── reload <name>             # Hot-reload agent config
│   └── logs <name>               # Stream agent logs
├── memory
│   ├── inspect <agent>           # Show MemAgent buffer contents
│   ├── search <agent> <query>    # Vector similarity search
│   ├── graph <agent>             # Show SYNAPSE graph stats
│   ├── compact                   # Run compaction manually
│   └── export <agent> <path>     # Export memory (JSON-LD)
├── tasks
│   ├── list                      # Active tasks
│   ├── status <id>               # Task state + DAG visualization
│   └── cancel <id>               # Cancel a task
├── deploy
│   ├── akash                     # Deploy to Akash
│   └── status                    # Deployment status
├── migrate
│   ├── ts-memory                 # Migrate TS 5-layer → LanceDB
│   └── validate                  # Validate migration
└── health                        # Runtime health check
```

**Key structs:**

```rust
#[derive(clap::Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    Dev(DevCommand),
    Agents(AgentsCommand),
    Memory(MemoryCommand),
    Tasks(TasksCommand),
    Deploy(DeployCommand),
    Migrate(MigrateCommand),
    Health,
}
```

**Dependencies (internal):** `af-core`, `af-actors`, `af-memory`, `af-transport`, `af-models`, `af-server`

**Dependencies (external):** `clap`, `tokio`, `tracing`, `serde_json`, `comfy-table` (terminal tables), `indicatif` (progress bars)

**Feature flags:** None — CLI is always full-featured.

**Estimated LoC:** ~2,500

---

### 2.11 af-server

**Purpose:** HTTP and gRPC server. axum for REST/WebSocket/SSE endpoints, tonic for gRPC, A2A endpoint mounting, Discord adapter, health checks.

**Public API Surface:**

```rust
pub struct AfServer {
    http: axum::Router,
    grpc: tonic::transport::Server,
    config: ServerConfig,
}

impl AfServer {
    pub async fn start(
        config: ServerConfig,
        router: Arc<MessageRouter>,
        a2a: Arc<A2aGateway>,
        orchestrator: Arc<WidthFirstScheduler>,
        security: Arc<SecurityClassifier>,
    ) -> AfResult<()>;
}

// === HTTP Routes ===
// GET  /health                        → HealthCheck
// GET  /metrics                       → Prometheus metrics
// GET  /.well-known/agent.json        → A2A Agent Card
// POST /api/v1/tasks                  → Submit task
// GET  /api/v1/tasks/:id              → Task status (SSE)
// POST /api/v1/tasks/:id/cancel       → Cancel task
// GET  /api/v1/agents                 → List agents
// GET  /api/v1/agents/:id/status      → Agent status
// POST /api/v1/agents/:id/reload      → Hot-reload config
// WS   /ws                            → WebSocket for real-time updates

// === Discord Adapter ===
pub struct DiscordAdapter {
    router: Arc<MessageRouter>,
    // Handles Discord message → NATS → agent → NATS → Discord reply
}

// === gRPC Services ===
// AgentService     — agent management RPCs
// TaskService      — task lifecycle RPCs
// MemoryService    — memory inspection RPCs
```

**Dependencies (internal):** `af-core`, `af-actors`, `af-transport`, `af-a2a`, `af-orchestrator`, `af-security`

**Dependencies (external):** `axum`, `tonic`, `tower`, `tower-http`, `tokio`, `prometheus`, `tracing`

**Feature flags:**
- `discord` (default) — Discord bot adapter
- `grpc` (default) — gRPC service
- `browser-wasm` — WASM-compatible client library (future)

**Estimated LoC:** ~3,000

---

### 2.12 af-napi (Migration Bridge)

**Purpose:** napi-rs bridge enabling the Rust runtime to be called from existing Node.js/TypeScript workers during the hybrid migration period. See [Section 5](#5-migration-bridge) for full architecture.

**Public API Surface (exposed to Node.js):**

```rust
#[napi]
pub fn start_runtime(config_path: String) -> napi::Result<RuntimeHandle>;

#[napi]
pub async fn send_task(handle: &RuntimeHandle, agent: String, payload: String) -> napi::Result<String>;

#[napi]
pub async fn query_memory(handle: &RuntimeHandle, agent: String, query: String) -> napi::Result<String>;

#[napi]
pub fn health_check(handle: &RuntimeHandle) -> napi::Result<HealthStatus>;
```

**Dependencies (internal):** `af-core`, `af-actors`, `af-memory`, `af-transport`, `af-models`

**Dependencies (external):** `napi`, `napi-derive`, `tokio`

**Estimated LoC:** ~800

---

## 3. Dependency Graph

### 3.1 Internal Crate Dependencies

```
                        af-core
                       /   |   \    \      \       \
                      /    |    \    \      \       \
                     v     v     v    v      v       v
            af-models  af-transport  af-sandbox  af-security  af-memory
                 |          |            |           |            |
                 |          |            |           |            |
                 v          v            v           v            v
                 +---------++-----------++----------++           |
                            |                                    |
                            v                                    |
                      af-actors <--------------------------------+
                       /     \
                      v       v
              af-orchestrator  af-a2a
                      |         |
                      v         v
                   af-server <--+
                      |
                      v
                   af-cli
                      |
                      v
                   af-napi (bridge)
```

### 3.2 Dependency Table

| Crate | Depends On (internal) |
|-------|----------------------|
| `af-core` | — (leaf) |
| `af-models` | `af-core` |
| `af-transport` | `af-core` |
| `af-sandbox` | `af-core` |
| `af-security` | `af-core` |
| `af-memory` | `af-core`, `af-models` (for EmbeddingEngine) |
| `af-actors` | `af-core`, `af-models`, `af-memory`, `af-transport`, `af-sandbox` |
| `af-orchestrator` | `af-core`, `af-actors`, `af-security` |
| `af-a2a` | `af-core`, `af-transport` |
| `af-server` | `af-core`, `af-actors`, `af-transport`, `af-a2a`, `af-orchestrator`, `af-security` |
| `af-cli` | `af-core`, `af-actors`, `af-memory`, `af-transport`, `af-models`, `af-server` |
| `af-napi` | `af-core`, `af-actors`, `af-memory`, `af-transport`, `af-models` |
| `af-migrate` | `af-core`, `af-memory` |
| `af-bench` | `af-core`, `af-actors`, `af-memory`, `af-transport` |

### 3.3 Build Order (topological sort)

```
Layer 0:  af-core
Layer 1:  af-models, af-transport, af-sandbox, af-security  (parallel)
Layer 2:  af-memory  (needs af-models)
Layer 3:  af-actors  (needs layers 0-2)
Layer 4:  af-orchestrator, af-a2a  (parallel, need af-actors)
Layer 5:  af-server  (needs layers 0-4)
Layer 6:  af-cli, af-napi  (need af-server)
Tools:    af-migrate, af-bench  (can build anytime after their deps)
```

---

## 4. Build Configuration

### 4.1 rust-toolchain.toml

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy", "miri", "llvm-tools-preview"]
targets = ["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]
```

### 4.2 .cargo/config.toml

```toml
[build]
# Use mold linker for fast debug builds (Linux)
# Install: apt install mold
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]

[target.x86_64-unknown-linux-musl]
# Static binary for Akash deployment
rustflags = ["-C", "target-feature=+crt-static"]

# Cross-compilation for macOS dev → Linux deploy
[target.aarch64-unknown-linux-gnu]
linker = "aarch64-linux-gnu-gcc"
```

### 4.3 Release Profile

```toml
[profile.release]
opt-level = 3
lto = "fat"           # Full LTO for smallest binary
codegen-units = 1     # Single codegen unit for max optimization
strip = true          # Strip debug symbols
panic = "abort"       # No unwinding in release (smaller binary)

[profile.dev]
opt-level = 0
debug = true
incremental = true

[profile.bench]
inherits = "release"
debug = true          # Keep debug info for profiling
strip = false

# CI profile: faster than release, still optimized
[profile.ci]
inherits = "release"
lto = "thin"          # Thin LTO for faster CI
codegen-units = 4     # Parallel codegen for faster CI
```

### 4.4 Proto Build (build.rs)

```rust
// af-core/build.rs
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                "../proto/agent.proto",
                "../proto/memory.proto",
                "../proto/transport.proto",
                "../proto/a2a.proto",
            ],
            &["../proto"],
        )?;
    Ok(())
}
```

### 4.5 CI Pipeline

```yaml
# .github/workflows/ci.yml
name: CI

on: [push, pull_request]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"

jobs:
  check:
    name: cargo check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo check --workspace --all-features

  clippy:
    name: clippy lint
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --all-targets --all-features -- -D warnings

  fmt:
    name: rustfmt
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt
      - run: cargo fmt --all --check

  test:
    name: cargo test
    runs-on: ubuntu-latest
    services:
      nats:
        image: nats:2-alpine
        ports: ["4222:4222"]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: taiki-e/install-action@cargo-nextest
      - run: cargo nextest run --workspace --all-features
      - run: cargo test --doc --workspace

  miri:
    name: miri (unsafe audit)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
        with:
          components: miri
      - run: cargo +nightly miri test -p af-core -p af-transport
        # Only run miri on crates that MIGHT have unsafe
        # (af-core proto types, af-transport Flume bridging)

  deny:
    name: cargo deny
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2

  bench:
    name: benchmarks (no regression)
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo bench --workspace --no-run
        # Compile only — full bench runs nightly
      # Nightly: full criterion benchmarks with historical comparison

  coverage:
    name: code coverage
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: llvm-tools-preview
      - uses: taiki-e/install-action@cargo-llvm-cov
      - run: cargo llvm-cov --workspace --lcov --output-path lcov.info
      - uses: codecov/codecov-action@v4
        with:
          file: lcov.info

  build-release:
    name: release build (linux-musl)
    runs-on: ubuntu-latest
    needs: [check, clippy, test]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl
      - run: cargo build --release --target x86_64-unknown-linux-musl -p af-server -p af-cli
      - uses: actions/upload-artifact@v4
        with:
          name: af-swarm-linux
          path: |
            target/x86_64-unknown-linux-musl/release/af-server
            target/x86_64-unknown-linux-musl/release/af-cli
```

### 4.6 deny.toml (License + Advisory Audit)

```toml
[advisories]
vulnerability = "deny"
unmaintained = "warn"
yanked = "deny"

[licenses]
allow = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "Zlib",
]
copyleft = "deny"

[bans]
multiple-versions = "warn"
wildcards = "deny"
deny = [
    # Reject crates with known safety issues
    { crate = "kanal", reason = "Pre-release, past UB bug — use flume instead" },
]
```

---

## 5. Migration Bridge

### 5.1 The Problem

The current system is 100% TypeScript. A full rewrite is high-risk (see [chief-architect-review.md, Risk 1](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md)). The hybrid approach: Rust core handles memory, routing, orchestration, and security. Node.js workers continue spawning Claude CLI subprocesses during migration.

### 5.2 Hybrid Runtime Architecture

```
                    ┌─────────────────────────────────────────────────┐
                    │              Rust Core (af-swarm)                │
                    │                                                  │
                    │  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │
                    │  │ Message  │  │ Memory   │  │ Security     │  │
                    │  │ Router   │  │ Engine   │  │ Guardrails   │  │
                    │  │ (Flume)  │  │ (Lance   │  │ (Capability  │  │
                    │  │          │  │  +SYNAPSE │  │  + Injection │  │
                    │  │          │  │  +MemAgt) │  │  + Budget)   │  │
                    │  └────┬─────┘  └─────┬────┘  └──────┬───────┘  │
                    │       │              │               │          │
                    │       └──────────────┼───────────────┘          │
                    │                      │                          │
                    │               ┌──────┴──────┐                  │
                    │               │  af-napi    │                  │
                    │               │  (N-API     │                  │
                    │               │   bridge)   │                  │
                    │               └──────┬──────┘                  │
                    └──────────────────────┼──────────────────────────┘
                                           │
                              ┌────────────┴────────────┐
                              │  Node.js Worker Process  │
                              │                          │
                              │  const runtime =         │
                              │    require('af-napi');    │
                              │                          │
                              │  // Memory: Rust-managed  │
                              │  const ctx = await        │
                              │    runtime.queryMemory(   │
                              │      'senku', input);     │
                              │                          │
                              │  // LLM: still via CLI    │
                              │  const result =           │
                              │    execFile('claude',     │
                              │      ['-p', prompt+ctx]); │
                              │                          │
                              │  // Store: Rust-managed   │
                              │  await runtime.storeMemory│
                              │    ('senku', result);     │
                              └──────────────────────────┘
```

### 5.3 napi-rs Bridge API

The `af-napi` crate exposes a minimal surface to Node.js:

```javascript
// Usage from existing TypeScript workers
const afSwarm = require('@alternatefutures/af-napi');

// 1. Start the Rust runtime (done once at worker boot)
const runtime = afSwarm.startRuntime('/path/to/config.toml');

// 2. Query memory (replaces file-based memory reads)
const memories = await afSwarm.queryMemory(runtime, 'senku', 'deployment architecture');
// Returns: JSON array of relevant memories from LanceDB + SYNAPSE

// 3. Store memory (replaces JSONL appends)
await afSwarm.storeMemory(runtime, 'senku', {
  content: 'Deployed auth service to Akash DSEQ 24756776',
  type: 'episodic',
  qualityScore: 0.3,
});

// 4. Check budget before making LLM call
const allowed = afSwarm.checkBudget(runtime, 'senku', estimatedTokens);
if (!allowed) { /* skip or queue */ }

// 5. Record usage after LLM call
afSwarm.recordUsage(runtime, 'senku', { tokens: 1543, costUsd: 0.02 });

// 6. Health check
const health = afSwarm.healthCheck(runtime);
// Returns: { agents: 17, memory_vectors: 8500, nats: 'connected', uptime_secs: 3600 }
```

### 5.4 Migration Phases

```
Phase A: Hybrid (Weeks 1-4)
  - Rust core handles: memory, routing, budget, security checks
  - Node.js handles: Claude CLI spawning, Discord bot, NATS message handling
  - Both systems share NATS for cross-process messaging
  - af-napi bridge connects them in-process

Phase B: Rust Agents (Weeks 5-8)
  - Migrate 3 pilot agents to pure Rust (content-writer, market-intel, senku)
  - These agents use af-models (Rig) directly — no Claude CLI
  - Remaining 14 agents stay on Node.js via af-napi bridge
  - Compare output quality: Rust vs TS for same inputs

Phase C: Full Migration (Weeks 9-12)
  - Migrate remaining agents to Rust
  - Decommission Node.js workers
  - af-napi bridge deprecated (kept for emergency rollback)

Phase D: Cleanup (Week 13+)
  - Remove af-napi from workspace
  - Archive TS codebase
  - Single Rust binary on Akash
```

### 5.5 Rollback Strategy

At every phase boundary, the system can roll back:

| Phase | Rollback Action | Data Impact |
|-------|----------------|-------------|
| A → Pre-A | Stop Rust core, Node.js workers continue with file-based memory | LanceDB vectors preserved, TS reads from JSONL (no data loss) |
| B → A | Route pilot agents back to Node.js workers | Memory stays in LanceDB (accessible via af-napi) |
| C → B | Re-enable Node.js workers for non-pilot agents | No data loss — NATS queue groups handle rebalancing |
| D → C | Redeploy Node.js workers from archived code | Requires JSONL export from LanceDB (af-cli memory export) |

---

## 6. Revised 14-Phase Build Plan

**Total estimate: 480-560 agent-hours** (incorporates chief architect's 400-600h range, senku's memory simplification, DePIN dual-tier additions, and QA's expanded testing budget).

Each phase includes its own tests — testing is continuous, not a final phase.

### Phase 0: Scaffolding + Agent Config Schema (12h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Cargo workspace init | root | 1h | All crate stubs, Cargo.toml workspace config |
| af-core types, errors, config | af-core | 4h | Error taxonomy, AgentConfig schema, RuntimeConfig |
| Proto definitions | proto/ | 2h | agent.proto, transport.proto, memory.proto, a2a.proto |
| Agent config TOML schema + 3 pilot configs | af-core | 3h | senku.toml, content-writer.toml, market-intel.toml |
| CI pipeline setup | .github/ | 2h | cargo check, clippy, fmt, test, deny |

**Milestone:** `cargo check --workspace` passes. Three agent configs parse correctly.

### Phase 1: Core Runtime (40h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Ractor actor framework integration | af-actors | 8h | AgentActor impl, actor lifecycle, state machine |
| Supervision tree | af-actors | 6h | SwarmSupervisor, GroupSupervisor, restart strategies |
| BoundedMailboxWrapper | af-actors | 3h | Semaphore-based backpressure on Ractor's unbounded mailboxes |
| Actor registry (named lookup) | af-actors | 2h | register/lookup/list via Ractor's registry |
| Single model provider (Anthropic via Rig) | af-models | 6h | AnthropicProvider + ModelProvider trait |
| EmbeddingEngine (fastembed-rs) | af-models | 3h | all-MiniLM-L6-v2, embed/embed_batch |
| Failover chain + circuit breaker | af-models | 4h | FallbackProvider, CircuitBreaker |
| Config loading + ArcSwap hot-reload | af-actors | 4h | ArcSwap config, notify file watcher, SIGHUP |
| Unit tests for all above | all | 4h | Actor lifecycle, model mock, config parse |

**Milestone:** Single agent actor boots, receives a message, calls Claude API, returns response. Config hot-reload works.

### Phase 2: Messaging (20h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Flume local channels (L1) | af-transport | 3h | Replace kanal, sender/receiver per agent |
| tokio::broadcast fan-out (L2) | af-transport | 2h | Topic-based pub/sub within process |
| Message Router (local auto-detect) | af-transport | 4h | Route to Flume if local, NATS if remote |
| NATS bridge (async-nats + prost) | af-transport | 5h | NatsBridge, subject hierarchy (af.v1.*) |
| NATS JetStream persistence | af-transport | 3h | Durable task queues for crash recovery |
| Integration tests (local + NATS) | af-transport | 3h | Cross-agent messaging, NATS round-trip |

**Milestone:** Two agents exchange messages locally via Flume. NATS bridge forwards to external subscribers.

### Phase 3: Agent Definitions + Pilot Agents (25h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Agent config parser (TOML → AgentConfig) | af-core | 3h | Full schema with validation |
| Prompt assembler (system prompt + agent personality + context) | af-actors | 5h | Template engine for constructing LLM prompts |
| Skill/capability system | af-core, af-actors | 4h | ToolPermission enforcement per agent |
| Port 3 pilot agents | af-actors | 6h | senku, content-writer, market-intel — full configs |
| Mock LLM provider for testing | af-models | 3h | Deterministic canned responses |
| Behavioral equivalence tests | tests/ | 4h | Same input → qualitatively similar output vs TS system |

**Milestone:** 3 agents running with full personality, skills, and capability restrictions. Tests validate behavior matches TS system.

### Phase 4: Basic Security (P0 Guardrails) (30h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Capability-based permissions (S1) | af-security | 5h | CapabilityChecker, parent→child narrowing |
| Prompt injection defense (S3) | af-security | 6h | Trust tagger, canary tokens, output validator |
| Secret management integration (S4) | af-security | 4h | Infisical client via reqwest, in-memory secret store |
| Resource governor (S6) | af-security | 5h | Per-agent budgets, daily circuit breaker |
| Security classifier (TEE routing) | af-security | 4h | Pattern matcher, entropy detection, tier decision |
| Security test suite | tests/ | 6h | Injection attempts, capability violations, budget overflow |

**Milestone:** Agents cannot exceed budgets, tool outputs are trust-tagged, injection attempts are caught, secrets never leak to logs.

### Phase 5: Memory Engine (45h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| VectorStore trait + LanceVectorStore | af-memory | 6h | Insert, search, delete, compact |
| Single-writer actor pattern for LanceDB | af-memory | 3h | Serialize writes through one tokio task |
| SYNAPSE graph memory | af-memory | 12h | Node/edge model, spreading activation algorithm |
| MemAgent controller | af-memory | 8h | Fixed-size buffer, LRU+relevance eviction, summarize_on_evict |
| Compaction job | af-memory | 3h | Periodic LanceDB sweep (TTL + access + relevance) |
| MemoryEngine (unified API) | af-memory | 4h | retrieve/store pipeline: MemAgent → SYNAPSE → LanceDB |
| Memory migration tool (TS → Rust) | tools/af-migrate | 5h | JSONL/JSON → embeddings → LanceDB + SYNAPSE graph |
| Integration tests + benchmarks | af-memory, af-bench | 4h | Cold start, warm start, retrieval accuracy |

**Milestone:** Memory engine stores and retrieves vectors. Migration tool converts TS 5-layer files. MemAgent maintains O(1) working context.

### Phase 6: Tool System + Sandbox (35h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Wasmtime WASM sandbox | af-sandbox | 8h | Engine, linker, WASI capabilities, pooling allocator |
| bubblewrap/seccomp fallback | af-sandbox | 6h | Runtime detection, namespace isolation or BPF whitelist |
| ToolRegistry (dispatch by sandbox tier) | af-sandbox | 3h | WASM tools vs native tools routing |
| Filesystem tool (WASM) | af-sandbox | 3h | Read/write/glob via wasi:filesystem |
| HTTP fetch tool (WASM) | af-sandbox | 3h | reqwest via wasi:http, domain allowlist |
| Shell executor (native sandbox) | af-sandbox | 4h | tokio::process inside bubblewrap |
| MCP client (WASM or native) | af-sandbox | 4h | JSON-RPC over SSE/HTTP |
| Tool integration tests | tests/ | 4h | Sandbox escape attempts, timeout enforcement |

**Milestone:** Tools execute in WASM sandbox (filesystem, HTTP) or native sandbox (shell). Sandbox escape attempts fail.

### Phase 7: Basic API + Discord Adapter (30h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| axum HTTP server | af-server | 5h | Routes, middleware (CORS, tracing, compression) |
| REST API (/api/v1/*) | af-server | 5h | Tasks, agents, health endpoints |
| WebSocket endpoint | af-server | 3h | Real-time task updates |
| tonic gRPC services | af-server | 5h | AgentService, TaskService protos |
| Discord adapter | af-server | 6h | Message → NATS → agent → NATS → Discord reply |
| API versioning (URL path per lain review) | af-server | 2h | /api/v1/, /a2a/v1/ |
| API integration tests | tests/ | 4h | HTTP, WebSocket, gRPC round-trips |

**Milestone:** API serves requests. Discord bot routes messages to Rust agents. gRPC clients can manage agents.

### Phase 8: Orchestrator (35h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| DAG task planner | af-orchestrator | 8h | Parse complex task → dependency graph via petgraph |
| Width-first scheduler (W&D) | af-orchestrator | 8h | Maximize parallel branch execution |
| Context sharder | af-orchestrator | 4h | Split large contexts across sub-agents |
| MAST failure detector | af-orchestrator | 6h | 14 failure mode taxonomy, circuit breakers per agent |
| DePIN: Akash adapter | af-orchestrator | 5h | SDL generation, deploy/status/teardown via MCP |
| Orchestrator integration tests | tests/ | 4h | DAG execution, failure recovery, parallel scheduling |

**Milestone:** Complex tasks decompose into DAGs and execute across agents in parallel. Failures trigger circuit breakers.

### Phase 9: Observability (20h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Structured logging (tracing) | all crates | 4h | Span trees, correlation IDs across NATS boundary |
| Prometheus metrics | af-server | 4h | Agent latency, mailbox depth, budget usage, task throughput |
| OpenTelemetry traces | af-server | 4h | Distributed tracing across agent calls |
| Cost attribution | af-security | 3h | Per-agent, per-model token/cost tracking |
| Health check endpoint | af-server | 2h | Agent status, NATS connectivity, memory stats |
| Grafana dashboard config | docs/ | 3h | Pre-built dashboards for Prometheus metrics |

**Milestone:** Full observability stack. Distributed tracing across agent calls. Cost attribution per agent.

### Phase 10: Migration + Parallel Run (30h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| af-napi bridge | bridges/af-napi | 8h | napi-rs bindings for memory, routing, budget |
| Parallel run infrastructure | af-transport | 6h | NATS queue groups, traffic splitting (10/50/100%) |
| Behavioral comparison harness | tests/ | 6h | Same input → Rust vs TS output comparison |
| Port remaining 14 agents | af-actors | 6h | Config files + prompt templates |
| Rollback testing | tests/ | 4h | Verify clean rollback at each phase boundary |

**Milestone:** Rust and TS systems run in parallel. Traffic gradually shifts to Rust. Rollback verified.

### Phase 11: Advanced Security (P1-P2) (35h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Agent identity (S5 — Ed25519 + NKey) | af-security | 5h | Keypair generation, NATS NKey auth |
| Audit trail (S7 — hash chain) | af-security | 6h | AuditEntry, SHA-256 chain, verification |
| Model integrity (S8 — SHA-256 checksums) | af-security | 3h | Verify GGUF model files before loading |
| Network egress control (S9) | af-security | 4h | Domain allowlist, IP blocklist enforcement |
| Data isolation (S10) | af-security | 4h | Cross-agent memory access prevention |
| TEE orchestrator (Phala adapter) | af-orchestrator | 8h | TDX attestation flow, secret provisioning, mTLS |
| Security penetration tests | tests/ | 5h | Full adversarial testing of all 10 guardrails |

**Milestone:** All 10 security guardrails active. TEE routing to Phala for sensitive workloads.

### Phase 12: Advanced Memory + Orchestrator (40h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| SYNAPSE optimization (per-agent graph partitioning) | af-memory | 8h | Subgraph isolation for >50K nodes |
| LanceDB S3 backend | af-memory | 5h | Horizontal scaling via S3-compatible storage |
| Memory export/import (JSON-LD) | af-memory | 4h | Portable agent memory format |
| LAMaS critical path optimizer | af-orchestrator | 8h | 38-46% critical path reduction |
| DePIN: io.net adapter | af-orchestrator | 6h | REST API, Ray cluster management |
| DePIN: Aethir adapter | af-orchestrator | 5h | Enterprise API integration |
| Provider selection logic | af-orchestrator | 4h | Cost-aware routing, failover |

**Milestone:** Memory scales horizontally. Advanced orchestration optimizations active. Multi-provider DePIN.

### Phase 13: A2A Gateway (25h)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Agent Card generation | af-a2a | 3h | Auto-generate from AgentConfig |
| Task lifecycle (send/get/cancel) | af-a2a | 6h | Full A2A task state machine |
| SSE streaming | af-a2a | 3h | Real-time task updates via Server-Sent Events |
| Webhook push notifications | af-a2a | 4h | Subscription CRUD, delivery with retry |
| 4-layer rate limiting | af-a2a | 4h | IP, API key, agent, global limits |
| A2A integration tests | tests/ | 3h | External agent simulation |
| A2A CLI commands | af-cli | 2h | Agent card inspection, task submission |

**Milestone:** External agents can discover, submit tasks to, and stream results from AF agents via A2A protocol.

### Phase 14: CLI + Polish + Benchmarks (25h — not estimated in original, added per QA review)

| Task | Crate | Hours | Notes |
|------|-------|-------|-------|
| Full CLI implementation | af-cli | 8h | All subcommands (dev, agents, memory, tasks, deploy) |
| Agent REPL (interactive debugging) | af-cli | 4h | Send messages, inspect state, query memory |
| Benchmark suite | af-bench | 5h | Criterion benchmarks: message latency, memory retrieval, agent spawn |
| Load testing (100 concurrent agents) | af-bench | 4h | Validate performance targets under load |
| Akash SDL for production deployment | docs/ | 2h | Multi-service SDL with persistent volumes |
| Documentation (ADRs for key decisions) | docs/adr/ | 2h | Config format, memory simplification, sandbox choice |

**Milestone:** Production-ready CLI. Benchmarks validate performance claims. Akash SDL ready for deployment.

---

### Phase Summary Table

| Phase | Name | Hours | Cumulative | Crates Touched |
|-------|------|-------|-----------|---------------|
| 0 | Scaffolding + Config Schema | 12h | 12h | af-core, proto |
| 1 | Core Runtime | 40h | 52h | af-actors, af-models |
| 2 | Messaging | 20h | 72h | af-transport |
| 3 | Agent Definitions + Pilots | 25h | 97h | af-core, af-actors, af-models |
| 4 | Basic Security (P0) | 30h | 127h | af-security |
| 5 | Memory Engine | 45h | 172h | af-memory, af-migrate |
| 6 | Tool System + Sandbox | 35h | 207h | af-sandbox |
| 7 | Basic API + Discord | 30h | 237h | af-server |
| 8 | Orchestrator | 35h | 272h | af-orchestrator |
| 9 | Observability | 20h | 292h | all crates |
| 10 | Migration + Parallel Run | 30h | 322h | af-napi, af-transport |
| 11 | Advanced Security (P1-P2) | 35h | 357h | af-security, af-orchestrator |
| 12 | Advanced Memory + Orchestrator | 40h | 397h | af-memory, af-orchestrator |
| 13 | A2A Gateway | 25h | 422h | af-a2a, af-server |
| 14 | CLI + Polish + Benchmarks | 25h | 447h | af-cli, af-bench |
| **TOTAL** | | **447h** | | |

**Buffer (20% contingency for Rust learning curve):** 90h

**Grand total with buffer: 537h** (within the 400-600h range specified by the chief architect review)

### Critical Path

```
Phase 0 ──► Phase 1 ──► Phase 2 ──► Phase 3 ──► Phase 4
                                        │
                    ┌───────────────────┤ (can start after Phase 3)
                    ▼                   ▼
                 Phase 5            Phase 6
                    │                   │
                    └─────────┬─────────┘
                              ▼
                          Phase 7 ──► Phase 8 ──► Phase 9
                                                    │
                                                    ▼
            ┌──────────────────────────────── Phase 10
            │         (parallel run — all prior phases must be done)
            ▼
        Phase 11 ─── Phase 12 ─── Phase 13 ─── Phase 14
        (these can partially overlap)
```

**Critical path phases 0-9: ~292h** (MVP with 3 agents, full security, API, observability)
**Remaining phases 10-14: ~155h** (migration, advanced features, A2A, CLI polish)

### Milestones

| Milestone | After Phase | What's Working |
|-----------|-------------|---------------|
| **M1: First Agent** | Phase 3 | 1 agent receives message, calls Claude, returns response |
| **M2: Secure Runtime** | Phase 6 | 3 agents with security guardrails, sandboxed tools, memory |
| **M3: Production API** | Phase 9 | REST/gRPC/Discord API, observability, orchestration |
| **M4: Parallel Run** | Phase 10 | Rust + TS running side by side, traffic splitting |
| **M5: Full Migration** | Phase 12 | All agents on Rust, TS decommissioned |
| **M6: External Access** | Phase 13 | A2A gateway live for external agents |
| **M7: Release** | Phase 14 | CLI polished, benchmarked, documented, deployed on Akash |

---

## Sources

- [rust-swarm-runtime.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/rust-swarm-runtime.md) — Original architecture plan
- [chief-architect-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/chief-architect-review.md) — 14 blocking concerns, 400-600h timeline, Flume over kanal, Rig abstraction layer
- [senku-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-review.md) — 5→3 memory simplification, prost serialization, VectorStore trait, Wasmtime over nsjail, fastembed-rs, hot-reload
- [senku-memory-deep-dive.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/senku-memory-deep-dive.md) — MemAgent absorption of SimpleMem/FadeMem, eviction model, migration strategy
- [depin-tee-architecture.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/revisions/depin-tee-architecture.md) — DePIN provider adapters, dual-tier TEE/sandbox, security classifier
- [qa-engineer-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/qa-engineer-review.md) — 37 findings, expanded testing budget, state recovery, benchmark CI
- [lain-review.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/reviews/lain-review.md) — NATS subject hierarchy, API versioning, rate limiting, webhook support

---

*Document authored 2026-02-15. This is the implementation blueprint — use it to `cargo init` the project.*
