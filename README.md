# AF Swarm Runtime

High-performance, model-agnostic Rust agent runtime for the Alternate Futures platform. Hosts 1000+ concurrent virtual actors with tiered state, dedicated inference serving, and multi-substrate deployment (AWS stable tier + Akash execution tier).

## Repository scope

This repo contains **Tracks A, B, and D** of the v3 implementation per [`docs/architecture/implementation-kickoff-plan.md`](docs/architecture/implementation-kickoff-plan.md):

- **Track A — Swarm Runtime** (co-leads: Lain runtime-core, Senku deployment): ractor supervision tree, Flume topology, hydrate/freeze, Dapr pub/sub bindings, K8s + Akash SDL
- **Track B — State + Integrity** (Quinn): Turso L1 embedded libSQL + SurrealDB L2 Raft cluster, OCC with HMAC integrity, NATS JetStream/Core split
- **Track D — Inference** (Atlas): Ray Serve + vLLM + Kimi K2.5, dedicated single-tenant replicas, FHE inference path

**Track C — Security + RESTRICTED** (Argus) lives in a separate repo: [`alternatefutures/swarm-security`](https://github.com/alternatefutures/swarm-security).

## Layout

```
crates/
  af-swarm-runtime/     — Rust actor runtime (ractor + Flume)
  af-swarm-state/       — Tiered state client (Turso L1 + SurrealDB L2) with HMAC OCC
  af-inference-client/  — Inference client to Ray Serve / vLLM
infrastructure/
  k8s/swarm-runtime/    — K8s manifests: namespace, Dapr Components, Deployment, KEDA, NetworkPolicy
  akash/                — Akash SDLs for execution-tier pods
proto/af-swarm/         — gRPC schemas shared across components
docs/architecture/
  design/               — Design docs merged in v3: integration-contract, state-schema, dapr-deployment, inference-cluster, rust-runtime-internals, test-benchmark-plan
  implementation-kickoff-plan.md  — 3-track rollout plan (P0–P3)
  rust-swarm-runtime.md, rust-workspace-plan.md, testing-framework.md
docs/infrastructure/
  substrate-phase-a-capacity-planning.md  — AF B300 cluster sizing
```

## Provenance

Extracted from [`alternatefutures/admin`](https://github.com/alternatefutures/admin) on 2026-04-24 at commit `d394a011`. v3 design history (9 merged PRs: #137 #138 #139 #140 #141 #142 #143 #169 #170 + plan #172) is preserved in admin's git log.

## Status

**Phase P0 (design merge) — complete.** Moving to Phase P1 (foundation) on 2026-05-04. See [`docs/architecture/implementation-kickoff-plan.md`](docs/architecture/implementation-kickoff-plan.md) §3.

## Owners

- Track A runtime-core (A1): Lain
- Track A deployment (A2): Senku
- Track B state + integrity: Quinn
- Track D inference: Atlas
- Cross-cutting coordination: chief-architect role (Senku)

## PR rules

All rules from `alternatefutures/admin/CLAUDE.md` apply here. In particular:

- Every PR requires `wonderwomancode` + `mavisakalyan` as reviewers
- Every PR references a GitHub issue (`#N`) in branch name, commits, and body (`Closes #N`)
- Branch naming: `feat/issue-N-description` or `fix/issue-N-description`
- No direct commits to main

Interface changes to `af-swarm-state::RichStateClient` require **three-way sign-off** (Lain + Argus + Atlas) — this is the critical-path API per the kickoff plan.
