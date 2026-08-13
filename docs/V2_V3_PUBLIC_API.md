# Blend V2 and V3 Public API Comparison

This non-normative review aid compares public contract entry points and data
types in the frozen Blend v2 baseline with the Blend v3 candidate. The
[specification index](SYSTEM_SPEC.md), [v2 specification](V2_SYSTEM_SPEC.md),
and [v3 specification](V3_SYSTEM_SPEC.md) remain authoritative.

Signatures omit the Soroban `Env` argument. Parameter names are shortened only
where their meaning remains clear. Constructors are included because they are
part of each contract's public ABI.

Classifications have these meanings:

- **Unchanged:** The public signature and caller-facing role are inherited.
- **Extended:** V3 retains the operation but adds a tier selector, changes its
  return shape, or adds caller-visible behavior required by the v3 spec.
- **Replaced:** V3 substitutes a different public operation. There are no such
  entry points in the current backstop, pool, or pool-factory API.

Data-type examples use readable JSON-like notation, not Stellar CLI's encoded
JSON representation. Addresses and hashes are symbolic strings, integer
amounts are illustrative raw base units, and omitted generic containers
(`Vec`, `Map`, `Option`, and tuples) have their ordinary Soroban meanings.
Private storage-only types are excluded.

## Backstop

### Entry points

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| `__constructor(backstop_token, emitter, blnd_token, usdc_token, pool_factory, drop_list)` | `__constructor(blnd_usdc_token, blnd_xlm_token, emitter, blnd_token, usdc_token, xlm_token, pool_factory, drop_list)` | Extended | Binds and validates the three fixed backstop assets and both Comet LPs. [V3 §3.1](V3_SYSTEM_SPEC.md#31-asset-configuration) |
| `deposit(from, pool, amount) -> i128` | `deposit(tier, from, pool, amount) -> i128` | Extended | Selects one independently accounted tier. [V3 §3.2](V3_SYSTEM_SPEC.md#32-position-accounting) |
| `queue_withdrawal(from, pool, amount) -> Q4W` | `queue_withdrawal(tier, from, pool, amount) -> Q4W` | Extended | Queues shares in one tier under the aggregate queue bound. [V3 §3.3](V3_SYSTEM_SPEC.md#33-withdrawals) |
| `dequeue_withdrawal(from, pool, amount)` | `dequeue_withdrawal(tier, from, pool, amount)` | Extended | Restores queued shares in one tier. [V3 §3.3](V3_SYSTEM_SPEC.md#33-withdrawals) |
| `withdraw(from, pool, amount) -> i128` | `withdraw(tier, from, pool, amount, to) -> i128` | Extended | Selects the tier and makes the withdrawal recipient explicit. [V3 §3.3](V3_SYSTEM_SPEC.md#33-withdrawals) |
| `user_balance(pool, user) -> UserBalance` | `user_balance(tier, pool, user) -> UserBalance` | Extended | Returns one pool-user balance for one tier. [V3 §3.2](V3_SYSTEM_SPEC.md#32-position-accounting) |
| `pool_data(pool) -> PoolBackstopData` | Same | Extended | Replaces the single-LP fields with three tier summaries, aggregate active USDC value, and value-weighted Q4W. [V3 §3.2](V3_SYSTEM_SPEC.md#32-position-accounting) |
| `backstop_token() -> Address` | `backstop_token(tier) -> Address` | Extended | Resolves the immutable token for the selected tier. [V3 §3.1](V3_SYSTEM_SPEC.md#31-asset-configuration) |
| `reward_zone() -> Vec<Address>` | Same | Unchanged | Keeps the v2 view; membership uses v3 activation value and BLND-bearing weight. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `distribute() -> i128` | Same | Extended | Keeps the v2 checkpoint surface while validating migration and allocating tier-aware emissions. [V3 §6.1](V3_SYSTEM_SPEC.md#61-migration-lifecycle-and-backfill--extended) |
| `gulp_emissions(pool) -> i128` | Same | Extended | Retains the 70/30 gulp while scheduling the eligible tier streams. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `add_reward(to_add, to_remove)` | Same | Extended | Admission and replacement use v3 activation and underlying-BLND rules. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `remove_reward(to_remove)` | Same | Extended | Removal uses the v3 activation threshold and zero-BLND exception. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `claim(from, pools, min_lp_out) -> i128` | `claim(tier, from, pools, min_lp_out) -> i128` | Extended | Compounds one eligible BLND-bearing tier across the selected pools. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `drop()` | Same | Extended | Keeps the v2 drop surface while verifying and funding the scheduled migration backfill. [V3 §6.1](V3_SYSTEM_SPEC.md#61-migration-lifecycle-and-backfill--extended) |
| `draw(pool, amount, to)` | `draw(tier, pool, amount, to)` | Extended | A pool draws loss capital from the tier selected by the waterfall. [V3 §5.2](V3_SYSTEM_SPEC.md#52-bad-debt-waterfall) |
| `donate(from, pool, amount)` | `donate(tier, from, pool, amount)` | Extended | Credits auction proceeds or voluntary recapitalization to one tier. [V3 §5.3](V3_SYSTEM_SPEC.md#53-take-rate-allocation--replaced) |

### Data types

| Type | V2 example | V3 example | Comparison |
| --- | --- | --- | --- |
| `Q4W` | <pre><code>{<br>  "amount": 100000000,<br>  "exp": 1800000000<br>}</code></pre> | Same | Shape is unchanged. V3 maintains an independent queue per tier under one aggregate pool-user entry bound. |
| `UserBalance` | <pre><code>{<br>  "shares": 900000000,<br>  "q4w": [<br>    {<br>      "amount": 100000000,<br>      "exp": 1800000000<br>    }<br>  ]<br>}</code></pre> | Same shape, returned for the selected `tier` | `shares` excludes queued shares in both versions. |
| `BackstopTier` | Not present | One of `"BlndXlm"`, `"BlndUsdc"`, or `"Usdc"` | New public selector for a fixed v3 backstop asset. Enum declaration order is not the loss-waterfall order. |
| `PoolTierData` | Not present | <pre><code>{<br>  "tokens": 500000000,<br>  "shares": 500000000,<br>  "value": 500000000<br>}</code></pre> | New nested summary; `value` is the tier's canonical seven-decimal USDC value. |
| `PoolBackstopData` | <pre><code>{<br>  "tokens": 1000000000,<br>  "shares": 1000000000,<br>  "q4w_pct": 1000000,<br>  "blnd": 8000000000,<br>  "usdc": 200000000,<br>  "token_spot_price": 10000000<br>}</code></pre> | <pre><code>{<br>  "active_value": 900000000,<br>  "blnd_usdc": {<br>    "tokens": 300000000,<br>    "shares": 300000000,<br>    "value": 300000000<br>  },<br>  "blnd_xlm": {<br>    "tokens": 500000000,<br>    "shares": 500000000,<br>    "value": 500000000<br>  },<br>  "q4w_pct": 1000000,<br>  "usdc": {<br>    "tokens": 200000000,<br>    "shares": 200000000,<br>    "value": 200000000<br>  }<br>}</code></pre> | Replaced view shape. V3 reports all tiers, aggregate active value, and value-weighted Q4W instead of one LP's underlying reserves and spot price. |

## Pool

### Entry points

All pool entry-point signatures remain v2-compatible. An **Extended** row
therefore identifies changed caller-visible validation or tier-aware behavior,
not a signature change.

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| `__constructor(admin, name, oracle, backstop_take_rate, max_positions, min_collateral, backstop, blnd)` | Same | Unchanged | Pool construction retains the v2 ABI and role. |
| `propose_admin(new_admin)` | Same | Unchanged | Inherits the v2 two-step admin transfer. [V2 §4](V2_SYSTEM_SPEC.md#4-pool-lifecycle-and-administration) |
| `accept_admin()` | Same | Unchanged | Inherits the v2 two-step admin transfer. [V2 §4](V2_SYSTEM_SPEC.md#4-pool-lifecycle-and-administration) |
| `update_pool(backstop_take_rate, max_positions, min_collateral)` | Same | Unchanged | Retains the v2 mutable pool parameters. [V2 §4](V2_SYSTEM_SPEC.md#4-pool-lifecycle-and-administration) |
| `queue_set_reserve(asset, metadata)` | Same | Unchanged | Retains the v2 delayed reserve setup. [V2 §4](V2_SYSTEM_SPEC.md#4-pool-lifecycle-and-administration) |
| `cancel_set_reserve(asset)` | Same | Unchanged | Retains the v2 queued-reserve cancellation. [V2 §4](V2_SYSTEM_SPEC.md#4-pool-lifecycle-and-administration) |
| `set_reserve(asset) -> u32` | Same | Unchanged | Retains the v2 reserve finalization and index return. [V2 §4](V2_SYSTEM_SPEC.md#4-pool-lifecycle-and-administration) |
| `get_config() -> PoolConfig` | Same | Unchanged | Returns the v2-compatible pool configuration. |
| `get_admin() -> Address` | Same | Unchanged | Returns the current pool admin. |
| `get_reserve_list() -> Vec<Address>` | Same | Unchanged | Returns the reserve list in index order. |
| `get_reserve(asset) -> Reserve` | Same | Unchanged | Returns current reserve accounting. |
| `get_positions(address) -> Positions` | Same | Unchanged | Returns v2-compatible user positions. |
| `submit(from, spender, to, requests) -> Positions` | Same | Extended | Adds bounded requests, exact token deltas, and tier-aware auction settlement behind the v2 request ABI. [V3 §4.2](V3_SYSTEM_SPEC.md#42-pool-integration--safety-extensions) |
| `submit_with_allowance(from, spender, to, requests) -> Positions` | Same | Extended | Applies the same v3 safety checks to allowance-based submission. [V3 §4.2](V3_SYSTEM_SPEC.md#42-pool-integration--safety-extensions) |
| `flash_loan(from, flash_loan, requests) -> Positions` | Same | Extended | Applies the v3 request bound and exact-balance checks to flash-loan submission. [V3 §4.2](V3_SYSTEM_SPEC.md#42-pool-integration--safety-extensions) |
| `update_status() -> u32` | Same | Extended | Uses aggregate canonical USDC value and value-weighted Q4W. [V3 §4.1](V3_SYSTEM_SPEC.md#41-pool-status-valuation--extended) |
| `set_status(pool_status)` | Same | Extended | Retains v2 admin statuses but validates them against v3 valuation. [V3 §4.1](V3_SYSTEM_SPEC.md#41-pool-status-valuation--extended) |
| `gulp(asset) -> i128` | Same | Unchanged | Retains v2 reserve-credit reconciliation. |
| `gulp_emissions() -> i128` | Same | Extended | Retains the pool tranche gulp with v3 carry and registered-pool checks. [V3 §6.3](V3_SYSTEM_SPEC.md#63-pool-supply-and-borrow-emissions--extended) |
| `set_emissions_config(metadata)` | Same | Extended | Retains v2 configuration semantics with the v3 input bound. [V3 §6.3](V3_SYSTEM_SPEC.md#63-pool-supply-and-borrow-emissions--extended) |
| `claim(from, reserve_token_ids, to) -> i128` | Same | Extended | Retains direct pool claims with bounded unique identifiers and carry-preserving accounting. [V3 §6.3](V3_SYSTEM_SPEC.md#63-pool-supply-and-borrow-emissions--extended) |
| `get_reserve_emissions(reserve_token_id) -> Option<ReserveEmissionData>` | Same | Unchanged | Keeps the v2-compatible reserve-emission view; v3 carries remain internal. |
| `get_user_emissions(user, reserve_token_id) -> Option<UserEmissionData>` | Same | Unchanged | Keeps the v2-compatible user-emission view; v3 carries remain internal. |
| `new_auction(auction_type, user, bid, lot, percent) -> AuctionData` | Same | Extended | Keeps the generic v2 API while privately selecting and settling backstop tiers for types 1 and 2. An empty backstop-auction assertion means “accept the canonical set”; a nonempty assertion must match it and never chooses assets. Interest `lot` remains a required reserve input. [V3 §4.2](V3_SYSTEM_SPEC.md#42-pool-integration--safety-extensions) |
| `get_auction(auction_type, user) -> AuctionData` | Same | Unchanged | Returns the v2-compatible public auction projection; tier metadata is private. [V3 §5.1](V3_SYSTEM_SPEC.md#51-tier-auction-lifecycle) |
| `del_auction(auction_type, user)` | Same | Extended | Retains v2 stale deletion and releases any private tier selection. [V3 §5.1](V3_SYSTEM_SPEC.md#51-tier-auction-lifecycle) |
| `bad_debt(user)` | Same | Extended | Supplier default requires exhaustion of all three tiers. [V3 §5.2](V3_SYSTEM_SPEC.md#52-bad-debt-waterfall) |

### Data types

All types in this table have the same public fields and Soroban ABI shape in
v2 and v3. V3-only carries and tier-auction metadata remain private and do not
appear in these values.

| Type | V2 example | V3 example | Comparison |
| --- | --- | --- | --- |
| `PoolConfig` | <pre><code>{<br>  "oracle": "G_ORACLE",<br>  "min_collateral": 1000000000,<br>  "bstop_rate": 2000000,<br>  "status": 1,<br>  "max_positions": 20<br>}</code></pre> | Same | Pool configuration ABI is unchanged. |
| `ReserveConfig` | <pre><code>{<br>  "index": 0,<br>  "decimals": 7,<br>  "c_factor": 8000000,<br>  "l_factor": 9000000,<br>  "util": 7500000,<br>  "max_util": 9500000,<br>  "r_base": 100000,<br>  "r_one": 500000,<br>  "r_two": 2000000,<br>  "r_three": 10000000,<br>  "reactivity": 100000,<br>  "supply_cap": 10000000000000,<br>  "enabled": true<br>}</code></pre> | Same | Reserve setup ABI is unchanged. |
| `ReserveData` | <pre><code>{<br>  "d_rate": 1000000000000,<br>  "b_rate": 1000000000000,<br>  "ir_mod": 0,<br>  "b_supply": 10000000000,<br>  "d_supply": 2500000000,<br>  "backstop_credit": 50000000,<br>  "last_time": 1800000000<br>}</code></pre> | Same | Accrued reserve-data ABI is unchanged. |
| `Reserve` | <pre><code>{<br>  "asset": "G_USDC",<br>  "config": "&lt;ReserveConfig above&gt;",<br>  "data": "&lt;ReserveData above&gt;",<br>  "scalar": 10000000<br>}</code></pre> | Same | The nested reserve view is unchanged. |
| `Positions` | <pre><code>{<br>  "liabilities": {<br>    "0": 250000000<br>  },<br>  "collateral": {<br>    "1": 500000000<br>  },<br>  "supply": {<br>    "2": 100000000<br>  }<br>}</code></pre> | Same | Maps remain keyed by reserve index. |
| `Request` | <pre><code>{<br>  "request_type": 4,<br>  "address": "G_USDC",<br>  "amount": 250000000<br>}</code></pre> | Same | Example is a borrow request; request discriminants 0 through 9 are inherited. |
| `FlashLoan` | <pre><code>{<br>  "contract": "C_RECEIVER",<br>  "asset": "G_USDC",<br>  "amount": 1000000000<br>}</code></pre> | Same | Flash-loan argument ABI is unchanged. |
| `ReserveEmissionMetadata` | <pre><code>{<br>  "res_index": 0,<br>  "res_type": 1,<br>  "share": 70<br>}</code></pre> | Same | Example assigns relative weight 70 to reserve 0's bToken stream. |
| `ReserveEmissionData` | <pre><code>{<br>  "expiration": 1800604800,<br>  "eps": 1000000,<br>  "index": 25000000000000,<br>  "last_time": 1800000000<br>}</code></pre> | Same | V3 carry is deliberately omitted from the public view. |
| `UserEmissionData` | <pre><code>{<br>  "index": 25000000000000,<br>  "accrued": 120000000<br>}</code></pre> | Same | V3 user carry is deliberately omitted from the public view. |
| `AuctionData` | <pre><code>{<br>  "bid": {<br>    "G_USDC": 1200000000<br>  },<br>  "lot": {<br>    "C_BLND_USDC_LP": 1000000000<br>  },<br>  "block": 1234567<br>}</code></pre> | <pre><code>{<br>  "bid": {<br>    "G_USDC": 1200000000<br>  },<br>  "lot": {<br>    "C_BLND_XLM_LP": 1000000000<br>  },<br>  "block": 1234567<br>}</code></pre> | Shape is unchanged. These bad-debt examples show that v3 may expose the privately selected tier token in `lot`. |

## Pool factory

### Entry points

The pool-factory source and public ABI are unchanged between the frozen v2
baseline and the v3 candidate.

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| `__constructor(pool_init_meta)` | Same | Unchanged | Binds the immutable pool WASM hash, backstop, and BLND token through `PoolInitMeta`. |
| `deploy(admin, name, salt, oracle, backstop_take_rate, max_positions, min_collateral) -> Address` | Same | Unchanged | Deploys a pool with the v2-compatible constructor ABI. |
| `is_pool(pool_id) -> bool` | Same | Unchanged | Preserves the factory-registration boundary used by the backstop. |

### Data types

| Type | V2 example | V3 example | Comparison |
| --- | --- | --- | --- |
| `PoolInitMeta` | <pre><code>{<br>  "pool_hash": "HASH_POOL",<br>  "backstop": "C_BACKSTOP",<br>  "blnd_id": "C_BLND"<br>}</code></pre> | Same | Pool-factory constructor ABI is unchanged. |
