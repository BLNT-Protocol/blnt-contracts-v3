# Repository Instructions

These instructions apply to the entire repository.

## Mission

Build and evaluate the experimental Blend Protocol v3 contracts while
preserving Blend v2 behavior except for differences explicitly specified for
v3.

This is financial smart-contract software. Prefer explicit invariants,
checked arithmetic, conservative state transitions, bounded execution, and
regression tests over implicit assumptions.

## Sources of truth

Read these before changing contract behavior:

1. `docs/SYSTEM_SPEC.md`
2. `docs/V2_SYSTEM_SPEC.md`
3. `docs/V3_SYSTEM_SPEC.md`
4. Existing contract tests and the implementation being changed

`docs/SYSTEM_SPEC.md` defines how the specifications compose. Unstated v3
behavior inherits the frozen v2 baseline. Do not introduce a behavioral
difference unless `docs/V3_SYSTEM_SPEC.md` classifies and permits it.

If the written v2 specification conflicts with the frozen executable
baseline, preserve the executable behavior and correct the document. Do not
silently create a new v3 policy.

## Repository boundary

- This repository contains the production v3 contract source, compatibility
  crates, test mocks, and contract specifications, including candidate-side
  migration entry points and backfill accounting.
- Keep cross-version fixtures, deployment runners, and network evidence outside
  this repository.
- Test mocks and compatibility crates MUST NOT become privileged production
  dependencies or alternate protocol-control paths.
- Do not describe this candidate as audited, production-ready, or official.

## Runtime baseline

- Use Rust 1.91.1, Soroban SDK 27.0.3, Stellar Protocol 27, and
  `wasm32v1-none`.
- Keep the exact toolchain and dependency versions reproducible through
  `rust-toolchain.toml` and the committed lockfiles.
- Do not add SDK 22 or SDK 26 build lanes to the v3 workspace.
- Never disable release overflow checks.

## Contract guardrails

- Use fixed-point integer arithmetic. Do not use floating point.
- Use checked arithmetic or an explicitly specified saturation rule.
- Failed operations must not leave partially updated custody or accounting.
- Authentication must cover the actor whose balance, debt, shares, or
  entitlement changes.
- Verify exact token-balance deltas where the specification requires them.
- Fail closed on missing, stale, invalid, negative, or inconsistent valuation
  and liability data.
- Bound loops, collections, request counts, ledger footprints, and temporary
  storage lifetimes so critical operations remain executable under Soroban
  resource limits.
- A token, share, liability, emission, carry, or queued withdrawal must not be
  double-counted across tiers or accounting scopes.

Preserve these specified v3 boundaries unless the specification is changed
first:

- Exactly three immutable backstop tiers: BLND:USDC LP, BLND:XLM LP, and plain
  USDC.
- Strict loss order: BLND:XLM LP, BLND:USDC LP, plain USDC, then suppliers.
- One tier per auction, at most one active interest auction per pool and tier,
  and a 100-USDC tier eligibility minimum.
- Activation entry at 12,500 USDC and maintenance at 10,000 USDC.
- Take-rate weighting of `4:3:2` in loss order.
- A maximum-30-pool permissionless reward zone and a 70/30 BLND split.
- Ongoing BLND weight only for active, nonqueued underlying BLND in the two
  BLND-bearing tiers.
- Backstop BLND claims compound into the originating BLND-bearing tier and
  credit active shares to the same user and pool.
- Backstop value is derived only from current 80:20 Comet reserves, with
  BLND:USDC as the USDC anchor; do not add a backstop price oracle.
- No protocol-wide governance, multisig, administrator recovery, emergency
  override, privileged WASM replacement, or alternate upgrade path.

The normative specifications, not this summary, govern edge cases and exact
accounting.

## Development workflow

Before editing:

- Inspect `git status` and preserve unrelated or staged user changes.
- Identify the inherited v2 behavior and the explicit v3 rule affected.
- Find the tests enforcing the current behavior.
- Record unresolved economic or administrative choices instead of guessing.

For contract changes:

1. Add or update a focused regression test.
2. Run the targeted native test during iteration.
3. Run the full native and optimized-WASM checks before handoff.
4. Exercise boundary values, authorization failures, overflow, stale prices,
   queued withdrawals, and resource growth when relevant.

Standard verification:

```bash
cargo fmt --all --check
cargo fmt --manifest-path test-suites/fuzz/Cargo.toml --check
make test
cargo clippy --workspace --all-targets --locked --message-format short
cargo clippy --manifest-path test-suites/fuzz/Cargo.toml \
  --all-targets --locked --message-format short
```

`make test` builds the optimized contracts, enforces the backstop WASM-size
guard, runs the workspace tests, and checks the fuzz workspace.

## Repository hygiene

- Use `apply_patch` for authored file changes.
- Preserve AGPL-3.0 notices and imported-code provenance.
- Keep both workspace lockfiles committed.
- Do not commit `target/`, generated local artifacts, secrets, private keys, or
  environment files.
- Do not commit, push, tag, publish, deploy, or alter external protocol state
  unless the user explicitly requests it.
