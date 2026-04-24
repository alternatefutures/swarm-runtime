# CLAUDE.md — swarm-runtime

## Repository scope

This repo implements Tracks A, B, and D of the AF Swarm Runtime (v3). Track C (security) is in `alternatefutures/swarm-security`. Agent profiles, Discord/NATS workers, ops scripts remain in `alternatefutures/admin`.

## Implementation tracks

| Track | Scope | Lead |
|-------|-------|------|
| A1 | Rust actor runtime (ractor, Flume, hydrate/freeze) | Lain |
| A2 | Deployment substrate (K8s, Akash SDL, KEDA, OCI pipeline) | Senku |
| B | Tiered state + integrity (Turso L1, SurrealDB L2, OCC/HMAC, NATS split) | Quinn |
| D | Inference (Ray Serve, vLLM, Kimi K2.5, FHE path to Track C) | Atlas |

See `docs/architecture/implementation-kickoff-plan.md` for phase gates and exit criteria.

## Critical path

`af-swarm-state::RichStateClient` v0.1 API freeze (Phase P1 exit) blocks Tracks A, C (in swarm-security), and D. Any interface change requires three-way sign-off (Lain + Argus + Atlas) before merge.

## Workspace layout

- `crates/af-swarm-runtime/` — actor runtime binary + library
- `crates/af-swarm-state/` — **the critical-path crate**; state client consumed by all other tracks
- `crates/af-inference-client/` — inference transport to Ray Serve
- `proto/af-swarm/` — gRPC schemas (build via `build.rs` in state + inference-client crates)
- `infrastructure/k8s/swarm-runtime/` — K8s manifests (Dapr Component CRDs, Deployment, KEDA, NetworkPolicy)
- `infrastructure/akash/swarm-runtime.sdl` — Akash SDL, execution tier only

## Substrate policy

Per `docs/architecture/design/dapr-deployment.md` §10 and issue `alternatefutures/admin#168`:

- **Stable tier on AWS** (Phase A → AF bare-metal Phase B): SurrealDB L2 cluster, NATS JetStream durable streams, Dapr placement, AAV service
- **Execution tier on Akash**: swarm-runtime pods, dapr sidecars, Turso L1 embedded per pod

Never assume Akash-only deployment. Stable quorum / durable state stores require stable substrate.

## PR rules (inherited from admin/CLAUDE.md)

- Every PR: reviewers `wonderwomancode` + `mavisakalyan`
- Every PR: closes an issue via `Closes #N` in body
- Branch naming: `feat/issue-N-description` or `fix/issue-N-description`, lowercase, hyphens
- Every commit references issue number
- No direct commits to main

## Build + test

Not yet scaffolded — P1 foundation work starts 2026-05-04. Expected commands once workspace compiles:

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Dispatching agents to this repo

Code agents receive tasks via NATS `code.tasks.<handle>` (handles: `lain`, `senku`, `argus`, `quinn`, `atlas`, `yusuke`, `chief-architect`, `frontend-ux-specialist`, `qa-engineer`). Each agent has a per-repo workspace at `/home/ec2-user/workspaces/swarm-runtime-<handle>` on the EC2 worker — never share a single clone between agents.

Agents sign commits using their own git identity (`getAgentGitEnv()` in admin `agent-profiles.ts`). SSH deploy keys for this repo: ed25519 keys at `/home/ec2-user/.ssh/agents/af-<handle>`.

## Cross-repo references

When referencing content in `alternatefutures/swarm-security` or `alternatefutures/admin`, use absolute GitHub URLs, not relative paths. The three repos are checked out independently.

## Design-change RFC rule

Any design-doc revision that crosses the interface between tracks (e.g., edits to `integration-contract.md` §3 API surface, or `state-schema.md` column shapes) must be proposed as an RFC issue with a 48-hour comment window before merging. Chief-architect approval required to merge.
