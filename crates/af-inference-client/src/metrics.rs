//! Prometheus metrics for cost attribution and cluster observability.
//!
//! Implements the metric definitions from inference-cluster.md §9.
//!
//! CARDINALITY WARNING: agent_id × tenant_id × model can be very high.
//! Use agent_type aggregations for dashboards; agent_id only for debugging.
//! See design §9.1.
//!
//! Issue: alternatefutures/admin#132

use prometheus::{
    CounterVec, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry,
};

/// All cluster metrics — register once, use from anywhere.
pub struct ClusterMetrics {
    /// Total inference requests by outcome
    pub requests_total: IntCounterVec,
    /// Input tokens consumed
    pub tokens_in_total: IntCounterVec,
    /// Output tokens generated
    pub tokens_out_total: IntCounterVec,
    /// Estimated cost in USD (Counter of fractional USD × 1_000_000 for integer storage)
    pub cost_usd_micros_total: IntCounterVec,
    /// End-to-end inference latency (seconds)
    pub latency_seconds: HistogramVec,
    /// Time spent waiting in the cluster queue (seconds)
    pub queue_wait_seconds: HistogramVec,
    /// Claude budget remaining (tokens)
    pub budget_remaining_tokens: IntGaugeVec,
    /// Active vLLM sequences per replica
    pub active_sequences: IntGaugeVec,
    /// Circuit breaker state (0=CLOSED, 1=HALF_OPEN, 2=OPEN)
    pub circuit_breaker_state: IntGaugeVec,
}

impl ClusterMetrics {
    pub fn new(registry: &Registry) -> Result<Self, prometheus::Error> {
        let requests_total = IntCounterVec::new(
            Opts::new("inference_requests_total", "Total inference requests"),
            &["agent_type", "tenant_id", "model", "task_type", "sensitivity", "status"],
        )?;

        let tokens_in_total = IntCounterVec::new(
            Opts::new("inference_tokens_in_total", "Input tokens consumed"),
            &["agent_type", "tenant_id", "model"],
        )?;

        let tokens_out_total = IntCounterVec::new(
            Opts::new("inference_tokens_out_total", "Output tokens generated"),
            &["agent_type", "tenant_id", "model"],
        )?;

        let cost_usd_micros_total = IntCounterVec::new(
            Opts::new(
                "inference_cost_usd_micros_total",
                "Estimated cost in USD × 1,000,000 (integer counter)",
            ),
            &["agent_type", "tenant_id", "model"],
        )?;

        let latency_seconds = HistogramVec::new(
            HistogramOpts::new("inference_latency_seconds", "End-to-end inference latency")
                // Buckets from design §9.2: .05 .1 .25 .5 1 2.5 5 10
                .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["agent_type", "model", "task_type"],
        )?;

        let queue_wait_seconds = HistogramVec::new(
            HistogramOpts::new("inference_queue_wait_seconds", "Time waiting in cluster queue")
                .buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 5.0]),
            &["model"],
        )?;

        let budget_remaining_tokens = IntGaugeVec::new(
            Opts::new(
                "inference_budget_remaining_tokens",
                "Claude API budget remaining (tokens)",
            ),
            &["tenant_id", "model"],
        )?;

        let active_sequences = IntGaugeVec::new(
            Opts::new(
                "inference_cluster_active_sequences",
                "Currently active vLLM sequences per replica",
            ),
            &["model", "replica_id"],
        )?;

        let circuit_breaker_state = IntGaugeVec::new(
            Opts::new(
                "inference_circuit_breaker_state",
                "Circuit breaker state: 0=CLOSED, 1=HALF_OPEN, 2=OPEN",
            ),
            &["model"],
        )?;

        registry.register(Box::new(requests_total.clone()))?;
        registry.register(Box::new(tokens_in_total.clone()))?;
        registry.register(Box::new(tokens_out_total.clone()))?;
        registry.register(Box::new(cost_usd_micros_total.clone()))?;
        registry.register(Box::new(latency_seconds.clone()))?;
        registry.register(Box::new(queue_wait_seconds.clone()))?;
        registry.register(Box::new(budget_remaining_tokens.clone()))?;
        registry.register(Box::new(active_sequences.clone()))?;
        registry.register(Box::new(circuit_breaker_state.clone()))?;

        Ok(Self {
            requests_total,
            tokens_in_total,
            tokens_out_total,
            cost_usd_micros_total,
            latency_seconds,
            queue_wait_seconds,
            budget_remaining_tokens,
            active_sequences,
            circuit_breaker_state,
        })
    }

    /// Record a completed inference request from an InferenceResponse.
    pub fn record(
        &self,
        agent_type: &str,
        tenant_id: &str,
        model: &str,
        task_type: &str,
        sensitivity: &str,
        status: &str,
        tokens_in: u64,
        tokens_out: u64,
        cost_usd: f64,
        latency_s: f64,
        queue_wait_s: f64,
    ) {
        self.requests_total
            .with_label_values(&[agent_type, tenant_id, model, task_type, sensitivity, status])
            .inc();
        self.tokens_in_total
            .with_label_values(&[agent_type, tenant_id, model])
            .inc_by(tokens_in);
        self.tokens_out_total
            .with_label_values(&[agent_type, tenant_id, model])
            .inc_by(tokens_out);
        self.cost_usd_micros_total
            .with_label_values(&[agent_type, tenant_id, model])
            .inc_by((cost_usd * 1_000_000.0) as u64);
        self.latency_seconds
            .with_label_values(&[agent_type, model, task_type])
            .observe(latency_s);
        self.queue_wait_seconds
            .with_label_values(&[model])
            .observe(queue_wait_s);
    }
}
