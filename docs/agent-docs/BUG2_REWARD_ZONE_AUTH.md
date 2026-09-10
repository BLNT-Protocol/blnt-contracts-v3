# BUG2 — Reward-zone membership is mutable by anyone, on an instantaneous threshold

**Verdict: confirmed, LOW-MEDIUM.** `add_reward` and `remove_reward` require no
authorization from anyone, and eviction turns on a single instantaneous
comparison with no hysteresis, grace period, or cooldown. Any address can evict
any pool the moment its `active_value` crosses below the activation threshold —
including a dip caused by ordinary market movement or by a depositor's own
legitimate withdrawal.

The permission model is **inherited from v2, not introduced by v3** (§4), which
is why this is not rated higher. What makes it worth a separate line item is that
it is the enabling permission for BUG1 §3.5, and it is exploitable on its own
with no price manipulation at all.

Scope: split out of `BUG1_AUCTION_MANIP.md` because the defect is a permission
and hysteresis question, not a valuation one, and its fix is independent.
Analysis on `main` at `f5e03b1`.

---

## 1. The defect

Neither entry point authenticates (`backstop/src/contract.rs:377,384`):

```rust
fn add_reward(e: Env, to_add: Address, to_remove: Option<Address>) {
    storage::extend_instance(&e);
    let removed = emissions::add_to_reward_zone(&e, to_add.clone(), to_remove);

fn remove_reward(e: Env, to_remove: Address) {
    storage::extend_instance(&e);
    emissions::remove_from_reward_zone(&e, to_remove.clone());
```

No `require_auth` on the pool, its depositors, or an admin.

`remove_from_reward_zone` (`backstop/src/emissions/manager.rs:74`) then gates
purely on a point-in-time read:

```rust
let valuation = build_pool_valuation(e, &to_remove);
if quote_activation(e, &valuation.active_values).meets_threshold {
    panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
}
require_distribute_run_recently(e);
reward_zone.remove(remove_index);
set_reward_zone(e, &reward_zone);
```

There is no time component anywhere in that path. A pool one stroop under the
threshold for one ledger is as evictable as a pool that has been dead for a
month. `require_distribute_run_recently` is not an obstacle — it checks
checkpoint freshness, and `distribute` is itself permissionless.

Three properties compound:

1. **No consent.** The pool cannot require its own signature for a membership
   change.
2. **No hysteresis.** `active_value` is a live Comet-derived valuation, so it
   moves with BLNT price. A pool at $13,000 needs roughly a 4% adverse move to
   become evictable, and it is evictable for exactly as long as that move lasts.
3. **Asymmetric cost.** Eviction needs only the threshold test. Re-entry needs a
   free slot, or out-weighing an incumbent (`manager.rs:60-64`). Evicting is
   always cheaper than returning.

## 2. Reproduction

`test-suites/tests/reward_zone_auth.rs`, passing. **No price manipulation** —
the pool is put under the threshold by its own depositor queueing a legitimate
withdrawal:

```
active_value at start = $62,500        (threshold $12,500)
active_value after queued withdrawal = $11,250
control: try_deposit under empty auth set -> Err
distribute() under empty auth set     -> Ok
remove_reward() under empty auth set  -> Ok
pool evicted; e.auths() is empty
```

The fixture calls `mock_all_auths()` (`test-suites/src/test_fixture.rs:67`),
which would mask a missing `require_auth`, so the test switches the environment
into enforcing mode with `set_auths(&[])` and carries a **positive control**:
`deposit`, which does authenticate its `from` argument, fails under exactly the
same conditions. Only then is `remove_reward` shown to succeed.

> This control matters. The eviction PoC in `auction_manip_eviction.rs` was
> originally described as demonstrating permissionlessness; under
> `mock_all_auths` it does not, and that comment has been corrected to point
> here. Membership eviction being unauthenticated is established by this test
> and by the source above, not by that one.

## 3. Why it stands alone

BUG1 §3.5 uses a manipulated valuation to force the threshold crossing. This
finding needs no manipulation: any honest crossing is enough, and honest
crossings are routine. A depositor queues a withdrawal, BLNT drifts down a few
percent, a large LP exits the anchor — each produces a window in which a
stranger can permanently remove the pool from emissions.

The pool operator has no defence available. They cannot withhold consent, and
there is no cooldown to top the backstop back up within. Their only recourse is
to re-add after the fact, which may be blocked if the zone has since filled.

## 3.1 Which pools are exposed

The two properties separate cleanly by backstop composition.

**No consent (property 1) applies to every pool.** It is a permission fact and
has nothing to do with valuation: any address can call `remove_reward` against
any member that is under threshold, however it got there.

**No hysteresis (property 2) applies only to pools holding a BLNT-bearing
tier.** `BUG1 §3.6` and `usdc_only_backstop_immunity.rs` show that a backstop
configured `Usdc`-only takes the `unit_pool_tier_valuation` pass-through and has
no Comet input: its `active_value` moved zero under an anchor deflation that cut
the stock fixture pool to 16%. Such a pool's threshold gate is not volatile, so
it crosses only when tokens genuinely move, and there is no window for a stranger
to exploit that the depositors did not create themselves.

That makes a USDC-only backstop a genuine mitigation for threshold stability —
with the caveat, from BUG1 §3.6, that such a pool also has zero BLNT emission
weight and so has little reason to be in the reward zone at all.

## 4. v2 comparison — inherited

Both properties are present in v2 and were carried forward unchanged:

- `blend-contracts-v2/backstop/src/contract.rs:281,288` — `add_reward` and
  `remove_reward` likewise call no `require_auth`.
- `blend-contracts-v2/backstop/src/backstop/pool.rs:105` —
  `is_pool_above_threshold` is also a live Comet-derived test
  (`load_pool_backstop_data:23` reads `get_total_supply`/`get_balance` on the
  Comet pool at call time), also with no hysteresis.

So this is not a v3 regression, and it should not be triaged as one. v2's gate
is a per-pool product constant (`blnd^4 * usdc >= 1e25`) over that pool's own LP
underlying; v3's is an aggregate `active_value >= 12,500` across tiers. The v3
change widens *which* reserves move the gate (BUG1 §3.3's cross-quoting), but
the permission model and the absence of hysteresis are both v2 behaviour.

Per `AGENTS.md`, unstated v3 behaviour inherits the frozen v2 baseline, so
changing this is a **specification change, not a bug fix**, and needs
`docs/V3_SYSTEM_SPEC.md` updated first.

## 5. Mitigations

Ranked by cost, and none of them require touching the valuation path:

1. **Require the pool to be under threshold across two distribution
   checkpoints.** The checkpoint already exists in storage and is already read on
   this path. One extra persisted flag per pool converts an instantaneous test
   into a sustained one, which defeats both an honest transient dip and BUG1
   §3.5's single-transaction skew. Cheapest fix that addresses the real problem.
2. **Evict only below a buffered threshold.** Allow eviction under, say, 90% of
   the activation threshold, so a marginal dip does not qualify. One constant.
   Weaker than (1) — a large enough move still clears the buffer — but trivial.
3. **Queue evictions.** Record an intent, execute after a delay, and cancel if
   the pool recovers. Gives the operator a window to respond. More state.
4. **`require_auth` from the pool or a guardian.** Closes it completely, but
   changes the keeper model and diverges from v2 — the largest spec change here,
   and probably not worth it for this severity.

Mitigation (1) also closes BUG1 §3.5, which is the only leg of BUG1 that
`BUG1_AUCTION_MANIP.md` §6 still recommends acting on.

## 6. Files examined

`backstop/src/contract.rs`, `backstop/src/emissions/manager.rs`,
`backstop/src/emissions/policy.rs`, `backstop/src/backstop/pool.rs`,
`test-suites/src/test_fixture.rs`, `backstop/src/emissions/tier_accounting.rs`,
`blend-contracts-v2/backstop/src/contract.rs`,
`blend-contracts-v2/backstop/src/emissions/manager.rs`,
`blend-contracts-v2/backstop/src/backstop/pool.rs`, `AGENTS.md`.
