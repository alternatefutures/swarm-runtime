//! LanceDB table schema for agent embedding storage.
//!
//! LanceDB is the embedding vector store for the AF swarm runtime (v3.0 §4.3).
//! Embeddings are NOT stored in the Dapr state store — they live here.
//! AgentStateRecord.lancedb_record_ids holds references to LanceDB entries.
//!
//! Design: docs/architecture/design/state-schema.md §6
//! Issue: alternatefutures/admin#134
//!
//! ⚠️ OQ-S1 (OPEN): LanceDB is node-local. Agent migration between nodes requires
//! exporting and importing LanceDB entries. Migration protocol not yet designed.
//! See state-schema.md §6 OQ-S1.

use chrono::{DateTime, Utc};

/// Schema for the `agents_memory_index` LanceDB table.
///
/// One LanceDB database per runtime node, shared across all agents on that node.
/// Partitioned by (tenant_id, agent_id) for tenant isolation.
///
/// Table creation (using lancedb-rs API):
///   schema is registered via the AgentMemoryEntry arrow schema at table creation.
#[derive(Debug, Clone)]
pub struct AgentMemoryEntry {
    /// Primary key — ULID.
    pub id: String,

    /// Tenant ID for multi-tenant isolation.
    pub tenant_id: String,

    /// Agent ID that owns this memory entry.
    pub agent_id: String,

    /// Which conversation turn produced this entry (if applicable).
    pub turn_id: Option<String>,

    /// Source record type: "conversation_turn", "tool_output", "synapse_retrieval", etc.
    pub source_type: String,

    /// Text content of this memory entry (for summarize_on_evict and retrieval display).
    pub content: String,

    /// Embedding vector (all-MiniLM-L6-v2, 384 dimensions, cosine distance).
    /// Computed via fastembed-rs (v3.0 §12 tech stack: "fastembed-rs (ort)").
    pub vector: Vec<f32>,

    /// Relevance score [0.0, 1.0]. Updated on each MemAgent access.
    /// Used as a tie-breaker in the compaction GC job (v3.0 §4.3 Tier 2 eviction).
    pub relevance_score: f32,

    /// Number of times this entry has been retrieved.
    pub access_count: u32,

    /// Whether this entry is pinned (exempt from compaction GC).
    pub is_pinned: bool,

    /// DSE data class (string representation of DataClass enum, v3.0 §5.3.1).
    /// Preserved for taint propagation when this entry is retrieved into context.
    pub data_class: String,

    /// DSE sensitivity level (string representation of SensitivityLevel enum).
    pub sensitivity: String,

    pub created_at: DateTime<Utc>,
    pub last_accessed_at: DateTime<Utc>,
}

/// Embedding model configuration.
/// Must match the model used in the fastembed-rs initialization in the runtime.
pub struct EmbeddingModelConfig {
    /// Model name as recognized by fastembed-rs.
    pub model_name: &'static str,
    /// Output embedding dimensions.
    pub dimensions: usize,
    /// Distance metric used for ANN queries.
    pub distance_metric: DistanceMetric,
}

/// Distance metric for ANN similarity search.
#[derive(Debug, Clone, Copy)]
pub enum DistanceMetric {
    /// Cosine similarity (normalized dot product). Preferred for sentence embeddings.
    Cosine,
    /// L2 Euclidean distance.
    L2,
    /// Dot product (unnormalized).
    Dot,
}

/// The embedding model used for all agent memory entries.
/// all-MiniLM-L6-v2 as specified in v3.0 §12 tech stack.
pub const AGENT_MEMORY_EMBEDDING_MODEL: EmbeddingModelConfig = EmbeddingModelConfig {
    model_name: "sentence-transformers/all-MiniLM-L6-v2",
    dimensions: 384,
    distance_metric: DistanceMetric::Cosine,
};

/// LanceDB table name for agent memory entries.
pub const AGENTS_MEMORY_TABLE: &str = "agents_memory_index";

/// Build the LanceDB query for SYNAPSE spreading activation lookup.
///
/// SYNAPSE queries start from a seed set of entry IDs, computes their
/// centroid vector, and returns the top-k nearest neighbors in LanceDB.
/// This function computes the query vector (centroid) given seed vectors.
///
/// See v3.0 §4.3 for SYNAPSE spreading activation description.
pub fn compute_synapse_query_vector(seed_vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    if seed_vectors.is_empty() {
        return None;
    }
    let dims = seed_vectors[0].len();
    let n = seed_vectors.len() as f32;

    let centroid: Vec<f32> = (0..dims)
        .map(|i| seed_vectors.iter().map(|v| v[i]).sum::<f32>() / n)
        .collect();

    Some(centroid)
}
