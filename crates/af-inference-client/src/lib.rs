//! af-inference-client — gRPC client for the AF shared inference cluster
//!
//! This crate provides the Rust runtime's interface to the Ray Serve cluster
//! defined in docs/architecture/design/inference-cluster.md.
//!
//! # Usage (skeleton — not fully implemented)
//!
//! ```rust,no_run
//! use af_inference_client::{InferenceClusterClient, InferenceRequest, SensitivityLevel, TaskType};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = InferenceClusterClient::connect("https://inference.cluster.internal").await?;
//!
//!     let response = client.infer(InferenceRequest {
//!         request_id: uuid::Uuid::now_v7().to_string(),
//!         agent_id: "agent-abc123".to_string(),
//!         agent_type: "orchestrator".to_string(),
//!         tenant_id: "acme_corp".to_string(),
//!         task_type: TaskType::Orchestration as i32,
//!         sensitivity: SensitivityLevel::Internal as i32,
//!         system_prompt: "You are ...".to_string(),
//!         user_prompt: "Plan the following task...".to_string(),
//!         max_tokens: 4096,
//!         ..Default::default()
//!     }).await?;
//!
//!     println!("Response: {}", response.content);
//!     Ok(())
//! }
//! ```
//!
//! Issue: alternatefutures/admin#132

// Generated protobuf/tonic code (populated by build.rs at compile time)
pub mod generated {
    // Include prost/tonic generated code from OUT_DIR
    // tonic_include_proto! macro expands to include!(concat!(env!("OUT_DIR"), "/..."))
    tonic::include_proto!("af.inference.v1");
}

// Re-export generated types at crate root for convenience
pub use generated::{
    // Service clients
    inference_service_client::InferenceServiceClient,
    classification_service_client::ClassificationServiceClient,
    restricted_pod_gateway_client::RestrictedPodGatewayClient,

    // Request/response types
    InferenceRequest,
    InferenceResponse,
    InferenceStreamChunk,
    ClassifyRequest,
    ClassifyResponse,
    RestrictedInferenceRequest,
    ClusterHealthRequest,
    ClusterHealthResponse,
    ModelCapacity,
    RoutingMetadata,
    TokenUsage,
    KvNamespaceKey,

    // Enums
    SensitivityLevel,
    ModelId,
    TaskType,
    HardwareTier,
    EstimatedComplexity,
    InferenceError,
    InferenceErrorCode,
    CircuitBreakerState,
};

pub mod client;
pub mod budget;
pub mod metrics;
pub mod namespace;
