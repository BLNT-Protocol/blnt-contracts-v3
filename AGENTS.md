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

- Each pool immutably configures one to three positional backstop tiers from
  the canonical BLND:XLM LP, BLND:USDC LP, USDC, and XLM assets. Configured
  assets are unique, integer take-rate weights are 1 through 10 and strictly
  decrease by tier, and tier order is the strict loss waterfall before
  suppliers.
- One tier per auction and at most one active interest auction per pool.
  Interest requires at least 200 USDC; bad debt uses every positively valued
  tier before suppliers.
- A single inclusive 12,500-USDC activation threshold.
- Activation, pool status, reward-zone admission, take-rate allocation, and
  the loss waterfall use the same authorization-aware transferable values.
  Deauthorized plain USDC has zero value until reauthorized, while its shares,
  queues, and pending interest remain accounted for.
- Plain-USDC and plain-XLM interest-auction payments credit 99% to their tier
  and reserve an exact cumulative 1% for bounded swaps through the matching
  canonical BLND Comet followed by an exact BLND burn. Buyback failure must
  not unwind or block the completed interest settlement.
- A maximum-30-pool permissionless reward zone and a 70/30 BLND split.
- Ongoing BLND weight only for active, nonqueued underlying BLND held in the
  exact canonical BLND:USDC or BLND:XLM Comet LPs, regardless of tier position.
- Migration backfill starts with the first pre-replacement `distribute`, not
  with an emitter queue, and remains capped at 10 million BLND.
- A compatible BLND:XLM emitter queue must be attested no earlier than its
  final seven days, and local activation must occur within seven days after
  unlock.
- Backstop BLND claims compound into the originating canonical BLND-bearing
  tier and credit active shares to the same user and pool.
- Backstop value comes only from current canonical Comet reserves. BLND:USDC
  is the USDC anchor, canonical USDC is one-for-one, and canonical XLM is
  priced by the BLND reserve ratio between the two Comets. Backstop valuation
  has no oracle input.
- Exact canonical BLND LP interest-auction bids donate 100%. Plain-USDC and
  plain-XLM bids apply the failure-isolated 1% buy-and-burn rule above.
- A direct lending-reserve custody deficit is reconciled against that
  reserve's supplier rate and then its unpaid take-rate credit without
  creating backstop debt or touching another reserve. Complete supplier-value
  exhaustion sets the bToken exchange rate exactly to zero. New supply,
  collateral supply, borrowing, and flash loans stop below a 0.1 b_rate and
  resume once accrued interest restores at least that rate. Zero b_supply
  alone is a normal empty-reserve state and permits the first supply while
  b_rate remains at least 0.1; inherited liquidity and health checks continue
  to govern other actions. When both rate and supply are zero, new risk remains
  disabled but existing
  liabilities continue accruing on the ordinary 100%-utilization curve; all
  resulting interest and positive custody surplus are credited to the backstop
  because no supplier denominator remains. Repayment and liquidation remain
  available. Only bad debt left after ordinary collateral liquidation reaches
  the configured waterfall.
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
