//! InferenceClusterClient — high-level wrapper over tonic-generated stubs.
//!
//! Handles:
//! - Connection management (lazy connect, auto-reconnect via tonic Channel)
//! - Circuit breaker integration (see design §7.3)
//! - Retry with backpressure (see design §10.3)
//! - Deadline injection per task type (see design §10.3)
//!
//! SKELETON — types and structure defined; full implementation deferred.
//! Issue: alternatefutures/admin#132

use std::time::Duration;
use thiserror::Error;
use tonic::transport::Channel;

use crate::{
    CircuitBreakerState, ClusterHealthRequest, InferenceRequest, InferenceResponse,
    InferenceServiceClient,
};

/// Deadlines per task type, in seconds.
/// See design doc §10.3 — Failure Modes (FM-22).
pub const DEADLINE_CLASSIFICATION_S: u64 = 5;
pub const DEADLINE_STANDARD_S: u64 = 60;
pub const DEADLINE_ORCHESTRATION_S: u64 = 120;

/// Retry policy for CLUSTER_FULL and INTERNAL errors.
/// See design §10.3.
const MAX_RETRIES: u32 = 3;
const RETRY_BASE_MS: u64 = 50;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),

    #[error("cluster full — all model slots occupied")]
    ClusterFull,

    #[error("budget exhausted for tenant {tenant_id}")]
    BudgetExhausted { tenant_id: String },

    #[error("circuit breaker open for model {model}")]
    CircuitOpen { model: String },

    #[error("inference timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("max retries ({MAX_RETRIES}) exceeded")]
    MaxRetriesExceeded,
}

/// High-level client for the shared inference cluster.
///
/// Cheap to clone — the inner `Channel` is reference-counted.
#[derive(Clone)]
pub struct InferenceClusterClient {
    inner: InferenceServiceClient<Channel>,
    circuit_state: std::sync::Arc<std::sync::atomic::AtomicI32>,
}

impl InferenceClusterClient {
    /// Connect to the Ray Serve gRPC gateway.
    ///
    /// Uses lazy connect — the actual TCP connection is established on first RPC call.
    /// The `Channel` handles reconnection automatically.
    pub async fn connect(endpoint: &str) -> Result<Self, ClientError> {
        let channel = tonic::transport::Endpoint::new(endpoint.to_string())?
            .connect_lazy();
        Ok(Self {
            inner: InferenceServiceClient::new(channel),
            circuit_state: std::sync::Arc::new(
                std::sync::atomic::AtomicI32::new(CircuitBreakerState::Closed as i32),
            ),
        })
    }

    /// Send a unary inference request with retry and deadline injection.
    ///
    /// Deadline is determined by `task_type` field in the request.
    pub async fn infer(&self, request: InferenceRequest) -> Result<InferenceResponse, ClientError> {
        // TODO(#132): Inject deadline header based on request.task_type
        // TODO(#132): Check circuit breaker state before dispatch
        // TODO(#132): Implement retry loop (MAX_RETRIES, RETRY_BASE_MS exponential backoff)
        // TODO(#132): Map InferenceErrorCode → ClientError variants
        let response = self
            .inner
            .clone()
            .infer(tonic::Request::new(request))
            .await?
            .into_inner();
        Ok(response)
    }

    /// Check cluster health — used by the circuit breaker probe (design §7.3).
    pub async fn health(&self) -> Result<crate::ClusterHealthResponse, ClientError> {
        let response = self
            .inner
            .clone()
            .cluster_health(tonic::Request::new(ClusterHealthRequest {}))
            .await?
            .into_inner();
        Ok(response)
    }

    /// Returns the timeout duration for a given task type.
    pub fn deadline_for_task(task_type: i32) -> Duration {
        use crate::TaskType;
        match TaskType::try_from(task_type).unwrap_or(TaskType::Unspecified) {
            TaskType::Classification => Duration::from_secs(DEADLINE_CLASSIFICATION_S),
            TaskType::Orchestration | TaskType::Reasoning => {
                Duration::from_secs(DEADLINE_ORCHESTRATION_S)
            }
            _ => Duration::from_secs(DEADLINE_STANDARD_S),
        }
    }
}

// TODO(#132 / chief-architect #136): Confirm whether Dapr actors call `infer()` synchronously
// (blocking on the response) or use an async reply-to pattern via JetStream.
// If async reply-to: the client needs a `stream_infer()` wrapper that publishes the
// streaming response back to a NATS subject. See design §8.2 OQ-C.
