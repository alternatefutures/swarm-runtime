# Implementation Kickoff Plan — Rust Swarm Runtime (v3)

**Status:** Draft · **Issue:** #171 · **Supersedes:** none
**Design PRs referenced:** #137 #138 #139 #140 #141 #142 #143 #169 #170
**Last updated:** 2026-04-22

---

## 0. Purpose

The v3 design phase is complete. Nine design PRs cover the full stack — Rust actor runtime, Dapr substrate, tiered state, shared inference cluster, multi-tenant security, capacity planning, and Phase C attestation. This document defines **what gets built first, who owns it, and when each phase is allowed to proceed.**

This is a *plan*, not a spec. Detail lives in the design PRs. This plan owns:

- Merge-order prerequisites (which PRs must land before implementation starts)
- Track decomposition (three parallel streams)
- Phase gates (entry criteria, exit criteria)
- Dependency graph across tracks
- Risk register with escape hatches

---

## 1. Merge Prerequisite Stack (P-1 → P0)

No implementation starts until the design stack is merged. Order is driven by cross-PR reference chains:

| # | PR | Head SHA | Depends on | Merge gate |
|---|----|----------|------------|------------|
| 1 | #141 Integration contract | `73c9c6de` | — | §4.6 integrity, §3.1 NATS split, §9 OQ-9 addendum landed |
| 2 | #143 State schema | `83d5524c` | #141 | §10–13 Turso L1 + SurrealDB L2 already folded into base |
| 3 | #138 Security (FHE + RESTRICTED) | `6edfcb40` | #141 | §13 OQ-9 security assessment, FM-23/FM-24, SEC-012/013 |
| 4 | #139 Inference cluster | `a36fb381` | #138 | §14 interface spec, dedicated single-tenant replicas, TP=8/PP |
| 5 | #140 Dapr deployment | `c5e08f42` | #141, #143, #138 | 4 blockers resolved 2026-04-21 (verified) |
| 6 | #137 Rust runtime internals | `01b5e9fb` | #141, #143 | ractor supervision, Flume topology, hydrate/freeze |
| 7 | #142 Test + benchmark plan | `1d0519a3` | #137, #140 | 1000-agent harness, scale ladder |
| 8 | #169 Phase A capacity | `6a04d9f8` | #138, #140 | AF B300 cluster sizing |
| 9 | #170 Phase C attestation | `5d4cee45` | #138, #169 | AAV, TDX 1.5 + TDISP, 4h credential TTL |

**Merge cadence:** one per day in the order above, with a 24-hour soak between merges for downstream conflict checks. Expected full-stack merge window: **2026-04-23 → 2026-05-01**.

---

## 2. Track Decomposition

Four parallel implementation tracks. Tracks are decoupled at the interface-contract layer (PR #141) — ownership boundaries are strictly enforced, cross-track changes require a design-change RFC.

### Track A — Swarm Runtime

**Co-leads:** Lain (runtime-core) + Senku (deployment substrate) · **Scope:** Rust actor runtime + deployment substrate
**Design anchors:** #137 (runtime internals — Lain), #140 (Dapr substrate — Senku), #169 (capacity)

Two-lead model reflects actual authorship: Lain owns the ractor/Flume actor-core (hydrate/freeze, supervision tree, state-client integration); Senku owns the substrate wrapping it (Dapr, K8s, Akash SDL, KEDA). Interface between them: the `swarm-runtime` binary's boot contract.

- **A1 (Lain):** ractor supervision tree, Flume topology, per-agent message pipeline, hydrate/freeze protocol (cold-start < 1.5s, warm hydration < 250ms), Dapr pub/sub bindings
- **A2 (Senku):** K8s manifests, Akash SDL (execution tier), KEDA scale-to-zero, OCI image pipeline to ECR, substrate health checks

### Track B — State + Integrity

**Lead:** Quinn · **Scope:** Tiered state store, OCC protocol, NATS split
**Design anchors:** #141 (integration contract §3, §4.6), #143 (state schema)

- Turso L1 embedded libSQL per pod; PVC-backed actor.db
- SurrealDB L2 3-node Raft cluster (Phase A: AWS; Phase B: AF B300)
- OCC write/read protocol: `version` monotonic u64 + HMAC-SHA256 `integrity_hmac`
- NATS transport split: JetStream (durable: task/result/audit/lifecycle) + Core (advisory: broadcast/backpressure/heartbeat)
- `RichStateClient` Rust crate — sole API surface across Tracks A/C/D

### Track C — Security + RESTRICTED

**Lead:** Argus¹ · **Scope:** Multi-tenant boundary, RESTRICTED POD, attestation
**Design anchors:** #138 (security), #170 (Phase C attestation), #169 §security (capacity)

- FHE boundary enforcement between tenant pods (Concrete Rust, Kimi K2.5 inference path)
- RESTRICTED POD TEE isolation: Intel TDX 1.5 + TDISP (rejected SEV-SNP per #138 §4.3)
- AF Attestation Verifier (AAV) service — MRTD policy engine, 4h credential TTL
- Per-row HMAC enforcement at L1 layer (cross-boundary with Track B — interface via `RichStateClient`)
- SEC-012 (tenant key isolation) + SEC-013 (attestation credential rotation)

¹ *Role/authorship note:* Argus's roster role in `agent-profiles.ts` is "Observability & Infrastructure," but Argus authored both #138 and #170. Lain's roster role is "Chief Security Engineer" but Lain authored #137 (runtime-core). We honor the de facto authorship — Argus leads Track C — and treat the role labels as legacy. Revisit in agent-profiles cleanup (non-blocking).

### Track D — Inference

**Lead:** Atlas² · **Scope:** Shared inference cluster, model serving, FHE inference path
**Design anchors:** #139 (Ray Serve + vLLM + Kimi K2.5)

- Ray Serve deployment topology (dedicated single-tenant vLLM replicas, max_concurrent_seqs=16, node anti-affinity per #138 §4.5)
- vLLM config for Kimi K2.5 on B300: TP=8 intra-host NVLink5, PP across hosts, block_size=32, FP4/MX-FP4 quantization
- Model version pinning + rollout (Kimi K2.5 Modified MIT License compliance, per §2.1)
- FHE inference path integration with Track C (Concrete Rust hand-off)
- Cross-track interface: Track A dispatches inference tasks via NATS `agent.task.inference.*` (JetStream topic)

² *Role/authorship note:* Atlas's roster role is "Auth & Billing Specialist" but authored #139. Same legacy-label caveat as Track C — de facto authorship honored.

---

## 3. Phase Plan

### Phase P0 — Design merge (2026-04-23 → 2026-05-01)

**Entry:** #140 verified (done, 2026-04-21). Option B structural cleanup complete.
**Work:** Merge the 9 PRs in order above. Each merge triggers a downstream sync pass.
**Exit:** All 9 merged to main. Main branch contains full design stack. Tag: `v3-design-final`.
**Owner:** chief-architect.

### Phase P1 — Foundation (2026-05-04 → 2026-05-29, 4 weeks)

Four tracks start **in parallel**. All four have independent design anchors; coordination is interface-only.

**Track A (Lain + Senku):** ractor workspace scaffold, Flume topology skeleton, cold-start harness against stub state client (A1); K8s manifest scaffolding + OCI image pipeline (A2).
**Track B (Quinn):** `RichStateClient` crate with Turso L1 + SurrealDB L2 backends; OCC protocol with integrity HMAC; NATS JetStream/Core split clients.
**Track C (Argus):** AAV service skeleton (no crypto yet) + TDX attestation stub; FHE boundary scaffolding.
**Track D (Atlas):** Ray Serve cluster stand-up (Phase A AWS, 2-node) with stub Kimi K2.5 model; vLLM config validation on a single B300 dev node.

**Exit (all must hold to enter P2):**
- [ ] Track B `RichStateClient` publishes v0.1.0 to internal registry. API frozen.
- [ ] Track A passes single-agent hydrate/freeze round-trip against real `RichStateClient` (A1 + A2 integrated).
- [ ] Track C AAV issues a valid (stub) credential; RESTRICTED POD boots with TDX quote.
- [ ] Track D serves a stub model via Ray Serve; Track A can dispatch an inference task end-to-end via NATS.
- [ ] 1000-agent benchmark harness (#142) runs against stub runtime.

### Phase P2 — Integration (2026-06-01 → 2026-06-26, 4 weeks)

Tracks converge on a real end-to-end actor lifecycle.

**Track A:** Real Dapr pub/sub, KEDA scale-to-zero, multi-agent supervision tree (A1 + A2).
**Track B:** Checkpoint-on-deactivation, 3-node SurrealDB Raft in AWS staging, backpressure.
**Track C:** Real FHE boundary between two tenant pods, AAV credential rotation loop, RESTRICTED POD with real TDX.
**Track D:** Real Kimi K2.5 serving on Ray Serve, FHE inference path wired to Track C, dedicated single-tenant replicas with anti-affinity.

**Exit:**
- [ ] 100-agent run passes #142 correctness suite against real substrate (Phase A AWS + Akash).
- [ ] Cold-start p99 < 1.5s; warm hydration p99 < 250ms.
- [ ] Argus signoff on multi-tenant boundary proofs.
- [ ] Attestation credential rotates cleanly under load.
- [ ] Track D: inference request p99 < budget (per #139); FHE path passes Track C boundary test.

### Phase P3 — Scale + harden (2026-06-29 → 2026-07-31, ~5 weeks)

**All tracks:** 1000-agent soak test, chaos tests, failure mode coverage for FM-1 through FM-25, SEC-001 through SEC-013, cost measurement against #169 capacity model.

**Exit (v3 ship gate):**
- [ ] 1000-agent soak (72h) clean.
- [ ] p99 latency budgets met under max load.
- [ ] Argus + chief-architect joint sign-off.
- [ ] #170 Phase C attestation live for RESTRICTED tier (opt-in).
- [ ] Substrate migration plan (#168 Phase B) has committed hardware date.

---

## 4. Dependency Graph

```
P0: merge #141 → #143 → #138 → #139 → #140 → #137 → #142 → #169 → #170
                                                              │
P1:       ┌───────────────────────────────────────────────────┘
          │
   ┌──────┼──────┬──────┐
   ▼      ▼      ▼      ▼
Track A Track B Track C Track D
(runtime)(state)(security)(inference)
 L:L+S    L:Q    L:Ar     L:At
   │      │      │        │
   │   RichStateClient    │
   │   v0.1 frozen API    │
   │   (consumed by A/C/D)│
   │      │      │        │
   └──────┼──────┼────────┘
          ▼      ▼
P2: E2E integration (Phase A substrate: AWS + Akash staging)
                 │
                 ▼
P3:      1000-agent soak · FM/SEC coverage · ship gate
```

**Critical path:** Track B (`RichStateClient` API freeze) blocks Tracks A, C, and D. Delays to Track B cascade across three downstream tracks. Mitigation: Track B lead (Quinn) gets first-priority review on any interface change; interface changes require Lain/Argus/Atlas sign-off before merge.

---

## 5. Risk Register

| # | Risk | Likelihood | Impact | Mitigation / escape hatch |
|---|------|------------|--------|---------------------------|
| R1 | SurrealDB Raft quorum instability on AWS Phase A | Medium | High (blocks P2) | Multi-AZ deployment + fallback to 5-node; tested in Phase A capacity model (#169) |
| R2 | Turso L1 PVC portability on Akash pod rescheduling (**OQ-I7** open) | Medium | Medium | L2 hydration on L1 cold miss is already the design; verify cold-miss p99 < 3s in P1 |
| R3 | TDX 1.5 + TDISP hardware not available on Phase A AWS (metal.24xlarge limited) | Medium | High (blocks Track C P2) | AAV can stub attestation in staging; hold Track C P2 exit for hardware availability, run Track A/B P2 exit without it |
| R4 | FHE throughput below budget for Kimi K2.5 inference | Low | High | #138 §6 defines fallback to dedicated single-tenant replicas (non-FHE) with stronger pod isolation |
| R5 | KEDA scale-to-zero incomplete checkpoint on eviction (**FM-25**) | Medium | High (data loss) | #140 §6.2 defines terminationGracePeriodSeconds=60 + checkpoint-before-eviction flush; validate in P2 |
| R6 | NATS JetStream vs Core client-side topic_class drift | Low | Medium | Hard runtime assertion per #140 §4; add CI lint that greps for raw topic strings |
| R7 | Senku's #140 rework introduced `bare-metal bare-metal` typo (line 165) + new OQ-I7 | Low | Low | Fix in post-merge cleanup PR; OQ-I7 tracked in P1 exit checklist |

---

## 6. Non-Goals (v3)

Explicitly **not** in scope for this kickoff:

- Frontend / dashboard for agent observability (deferred to v4)
- Multi-region Raft (single-region Phase A; #168 Phase B covers cross-region)
- Akash execution for RESTRICTED tier (Phase A runs RESTRICTED on AWS only)
- Agent marketplace / billing integration (separate track)
- Self-service tenant onboarding (manual for P3 ship; automated in v4)

---

## 7. Communication Cadence

- **Weekly sync:** chief-architect + 3 track leads (Senku/Quinn/Argus), Thursdays.
- **Interface changes:** any API change in `RichStateClient` or #141 contract requires an RFC issue + 48h comment window + chief-architect approval before merge.
- **Phase gates:** exit checklist reviewed in a dedicated sync; no tacit rollover.
- **Incident channels:** #code for code-level issues, #updates for automated status (agents post summaries hourly).

---

## 8. Reviewer Checklist

- [ ] Merge order in §1 matches current dependency chain (spot-check 3 PRs' "Depends on" against their headers)
- [ ] Track ownership in §2 matches agent profiles and existing PR authorship
- [ ] P1/P2/P3 exit criteria are measurable (no "works well")
- [ ] Risk register covers all OQs that were left open in merged PRs
- [ ] Non-goals explicitly lists v4 punts

---

**Approvals required:** @wonderwomancode (CEO/CTO), @mavisakalyan (co-founder).
**Closes:** #171.
