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
- **Added:** V3 introduces an operation with no v2 counterpart.
- **Replaced:** V3 substitutes a different public operation. There are no such
  entry points in the current backstop, pool, or pool-factory API.

Data-type examples use readable JSON-like notation, not Stellar CLI's encoded
JSON representation. Addresses and hashes are symbolic strings, integer
amounts are illustrative raw base units, and omitted generic containers
(`Vec`, `Map`, `Option`, and tuples) have their ordinary Soroban meanings.
Private storage-only types are excluded.

## Emitter

V3 deploys a separate emitter for BLNT while the legacy emitter and BLND remain
with v1/v2. The established emitter entry points and encodings are retained;
the v3 emitter adds only constructor-bound initialization authority.

| Legacy entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| No constructor | `__constructor(initializer)` | Added | Prevents first-call initialization races without creating an ongoing administrator. |
| `initialize(blnd_token, backstop, backstop_token)` | Same ABI; `blnd_token` contains BLNT | Extended | Requires the constructor-bound one-time initializer, then configures BLNT emission to v3 and canonical BLNT:USDC as the initial designated token. |
| `distribute() -> i128` | Same | Extended | Mints BLNT at the inherited one-token-per-second rate and requires the current backstop; callers use the backstop's permissionless checkpoint. |
| `get_last_distro(backstop) -> u64` | Same | Unchanged | Preserves per-backstop distribution checkpoints. |
| `get_backstop() -> Address` | Same | Unchanged | Returns the current BLNT recipient backstop. |
| `queue_swap_backstop(new_backstop, new_backstop_token)` | Same | Unchanged | Preserves strict raw-balance qualification and the 31-day future-upgrade queue. |
| `get_queued_swap() -> Option<Swap>` | Same | Unchanged | Preserves the queued-swap view. |
| `cancel_swap_backstop()` | Same | Extended | Preserves invalid-queue cancellation and also permits cancellation through the same entry point after the seven-day execution window expires. |
| `swap_backstop()` | Same | Extended | Preserves final distribution, revalidation, and recipient replacement while limiting execution to the seven days after unlock. |
| `drop(list)` | Same | Extended | Mints the initial BLNT allocation under the inherited one-call, 50-million-token ceiling. |

## Backstop

### Entry points

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| `__constructor(backstop_token, emitter, blnd_token, usdc_token, pool_factory, drop_list)` | `__constructor(blnt_usdc_token, blnt_xlm_token, emitter, blnt_token, usdc_token, xlm_token, pool_factory, drop_list)` | Extended | Binds and validates the canonical BLNT assets and both emission-eligible Comet V2 LPs. The emitter's initial recipient may bind a 50-million-BLNT list; migration candidates are capped at 40 million to reserve the maximum 10-million-BLNT backfill within the emitter's unchanged aggregate ceiling. Pool-specific tiers come from the factory. [V3 §3.1](V3_SYSTEM_SPEC.md#31-asset-configuration) |
| `deposit(from, pool, amount) -> i128` | `deposit(tier, from, pool, amount) -> i128` | Extended | Selects one independently accounted tier. [V3 §3.2](V3_SYSTEM_SPEC.md#32-position-accounting) |
| `queue_withdrawal(from, pool, amount) -> Q4W` | `queue_withdrawal(tier, from, pool, amount) -> Q4W` | Extended | Queues shares in one tier under the aggregate queue bound. [V3 §3.3](V3_SYSTEM_SPEC.md#33-withdrawals) |
| — | `force_queue_withdrawal(tier, user, pool) -> Q4W` | Added | After backstop-deposit permission is revoked, queues all active shares in one tier only to the target's inherited Q4W. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |
| `dequeue_withdrawal(from, pool, amount)` | `dequeue_withdrawal(tier, from, pool, amount)` | Extended | Restores queued shares in one tier. [V3 §3.3](V3_SYSTEM_SPEC.md#33-withdrawals) |
| `withdraw(from, pool, amount) -> i128` | `withdraw(tier, from, pool, amount, to) -> i128` | Extended | Selects the tier and makes the withdrawal recipient explicit. [V3 §3.3](V3_SYSTEM_SPEC.md#33-withdrawals) |
| — | `force_withdrawal(tier, user, pool) -> i128` | Added | After Q4W maturity, withdraws all matured shares in one tier only to the permission-revoked target. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |
| `user_balance(pool, user) -> UserBalance` | `user_balance(tier, pool, user) -> UserBalance` | Extended | Returns one pool-user balance for one tier. [V3 §3.2](V3_SYSTEM_SPEC.md#32-position-accounting) |
| `pool_data(pool) -> PoolBackstopData` | Same | Extended | Replaces the single-LP fields with an ordered one-to-three-tier vector, aggregate transferable active USDC-equivalent value, and transferable-value-weighted Q4W. [V3 §3.2](V3_SYSTEM_SPEC.md#32-position-accounting) |
| `backstop_token() -> Address` | `backstop_token(tier, pool) -> Address` | Extended | Resolves the selected pool's immutable token at that waterfall position. [V3 §3.1](V3_SYSTEM_SPEC.md#31-asset-configuration) |
| `reward_zone() -> Vec<Address>` | Same | Unchanged | Keeps the v2 view; membership uses v3 activation value while allocation and full-zone replacement use eligible underlying BLNT. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `distribute() -> i128` | Same | Extended | Keeps the checkpoint surface, directly activates the fresh v3 emitter binding, and allocates tier-aware BLNT emissions. [V3 §6.1](V3_SYSTEM_SPEC.md#61-v3-emitter-launch-and-replacement--replaced-and-extended) |
| `gulp_emissions(pool) -> i128` | Same | Extended | Retains the 70/30 gulp while scheduling the eligible tier streams. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `add_reward(to_add, to_remove)` | Same | Extended | Admission uses v3 activation; full-zone replacement remains strictly underlying-BLNT weighted. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `remove_reward(to_remove)` | Same | Extended | Removal uses the v3 activation valuation and otherwise preserves the v2 threshold and checkpoint rules. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `claim(from, pools, min_lp_out) -> i128` | `claim(tier, from, pools, min_lp_out) -> i128` | Extended | Compounds one eligible BLNT-bearing tier across the selected pools. [V3 §6.2](V3_SYSTEM_SPEC.md#62-backstop-depositor-emissions--extended) |
| `drop()` | Same | Extended | Keeps the one-call drop surface for the immutable BLNT allocation and migration backfill. V3 launch may use the full 50-million-BLNT list because it schedules no backfill; a future migration combines its at-most-40-million list with at most 10 million of backfill. [V3 §6.1](V3_SYSTEM_SPEC.md#61-v3-emitter-launch-and-replacement--replaced-and-extended) |
| — | `buy_and_burn(asset) -> i128` | Added | Permissionlessly swaps one bounded pending USDC or XLM haircut batch through its canonical BLNT Comet and burns the exact BLNT output. [V3 §5.3](V3_SYSTEM_SPEC.md#53-take-rate-allocation--replaced) |
| `draw(pool, amount, to)` | `draw(tier, pool, amount, to)` | Extended | A pool draws loss capital from the tier selected by the waterfall. [V3 §5.2](V3_SYSTEM_SPEC.md#52-bad-debt-waterfall) |
| `donate(from, pool, amount)` | `donate(tier, from, pool, amount)` | Extended | Credits the full BLNT-LP-tier payment or 99% of a plain-USDC/plain-XLM payment after its buyback haircut. [V3 §5.3](V3_SYSTEM_SPEC.md#53-take-rate-allocation--replaced) |

### Data types

| Type | V2 example | V3 example | Comparison |
| --- | --- | --- | --- |
| `Q4W` | <code>{"amount":100000000, "exp":1800000000}</code> | Same | Shape is unchanged. V3 maintains an independent queue per tier under one aggregate pool-user entry bound. |
| `UserBalance` | <code>{"shares":900000000, "q4w":[{"amount":100000000, "exp":1800000000}]}</code> | Same shape, returned for the selected `tier` | `shares` excludes queued shares in both versions. |
| `BackstopTier` | Not present | One of `"FirstLoss"`, `"SecondLoss"`, or `"ThirdLoss"` | New public selector for a v3 loss-waterfall position. Enum declaration order is the loss-waterfall order. |
| `BackstopAsset` | Not present | One of `"BlntXlm"`, `"BlntUsdc"`, `"Usdc"`, or `"Xlm"` | The only canonical assets assignable to a v3 tier. |
| `PoolTierData` | Not present | <code>{"asset":"BlntXlm", "blnt_emission_eligible":true, "take_rate_weight":4, "token":"C_BLNT_XLM_LP", "tokens":500000000, "shares":500000000, "value":500000000}</code> | New nested summary; `value` is the tier's transferable, verified seven-decimal USDC-equivalent value. A deauthorized plain-USDC tier reports zero. |
| `PoolBackstopData` | <code>{"tokens":1000000000, "shares":1000000000, "q4w_pct":1000000, "blnd":8000000000, "usdc":200000000, "token_spot_price":10000000}</code> | <code>{"active_value":900000000, "q4w_pct":1000000, "tiers":[{"asset":"BlntXlm", "blnt_emission_eligible":true, "take_rate_weight":4, "token":"C_BLNT_XLM_LP", "tokens":500000000, "shares":500000000, "value":500000000}, {"asset":"Xlm", "blnt_emission_eligible":false, "take_rate_weight":2, "token":"C_XLM", "tokens":400000000, "shares":400000000, "value":400000000}]}</code> | Replaced view shape. V3 reports configured tiers, aggregate transferable active value, and transferable-value-weighted Q4W instead of one LP's underlying reserves and spot price. A deauthorized plain-USDC tier retains accounting but reports zero value. |

## Pool

### Entry points

Except for added v3 operations and the optional constructor controller, pool
entry-point signatures remain v2-compatible. An **Extended** row identifies
changed caller-visible validation or tier-aware behavior, not necessarily a
signature change.

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| `__constructor(admin, name, oracle, backstop_take_rate, max_positions, min_collateral, backstop, blnd)` | `__constructor(admin, name, oracle, backstop_take_rate, max_positions, min_collateral, backstop, blnt, access_controller)` | Extended | Uses BLNT for v3 emissions and optionally binds one immutable external access controller; `None` preserves permissionless behavior. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |
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
| — | `clawback(asset, from, amount)` | Added | Lets the reserve SAC administrator burn an exact clawbackable pool balance while removing the corresponding user's ordinary supply before collateral and invalidating an affected liquidation auction. [V3 §4.4](V3_SYSTEM_SPEC.md#44-reserve-clawback--added) |
| — | `reconcile_loss(asset) -> i128` | Added | Recognizes a direct reserve-custody deficit against the affected reserve's supplier rate and then unpaid take-rate credit without creating backstop debt; ordinary liquidation handles users made unhealthy by the haircut. [V3 §4.5](V3_SYSTEM_SPEC.md#45-reserve-loss-reconciliation--safety-extension) |
| — | `force_withdrawal(user, asset) -> i128` | Added | After supply permission is revoked for a debt-free user, burns all of that user's bTokens for one reserve and returns the exact underlying only to that user. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |
| — | `new_forced_exit_auction(user) -> AuctionData` | Added | After borrow permission is revoked, creates a caller-unparameterized auction for all target liabilities and proportionally required collateral. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |
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
| `bad_debt(user)` | Same | Extended | Supplier default requires exhaustion of every configured tier. [V3 §5.2](V3_SYSTEM_SPEC.md#52-bad-debt-waterfall) |

### Data types

All types in this table have the same public fields and Soroban ABI shape in
v2 and v3. V3-only carries and tier-auction metadata remain private and do not
appear in these values.

| Type | V2 example | V3 example | Comparison |
| --- | --- | --- | --- |
| `PoolConfig` | <code>{"oracle":"G_ORACLE", "min_collateral":1000000000, "bstop_rate":2000000, "status":1, "max_positions":20}</code> | Same | Pool configuration ABI is unchanged. |
| `ReserveConfig` | <code>{"index":0, "decimals":7, "c_factor":8000000, "l_factor":9000000, "util":7500000, "max_util":9500000, "r_base":100000, "r_one":500000, "r_two":2000000, "r_three":10000000, "reactivity":100000, "supply_cap":10000000000000, "enabled":true}</code> | Same | Reserve setup ABI is unchanged. |
| `ReserveData` | <code>{"d_rate":1000000000000, "b_rate":1000000000000, "ir_mod":0, "b_supply":10000000000, "d_supply":2500000000, "backstop_credit":50000000, "last_time":1800000000}</code> | Same | Accrued reserve-data ABI is unchanged. |
| `Reserve` | <code>{"asset":"G_USDC", "config":"&lt;ReserveConfig above&gt;", "data":"&lt;ReserveData above&gt;", "scalar":10000000}</code> | Same | The nested reserve view is unchanged. |
| `Positions` | <code>{"liabilities":{"0":250000000}, "collateral":{"1":500000000}, "supply":{"2":100000000}}</code> | Same | Maps remain keyed by reserve index. |
| `Request` | <code>{"request_type":4, "address":"G_USDC", "amount":250000000}</code> | Same | Example is a borrow request; request discriminants 0 through 9 are inherited. |
| `FlashLoan` | <code>{"contract":"C_RECEIVER", "asset":"G_USDC", "amount":1000000000}</code> | Same | Flash-loan argument ABI is unchanged. |
| `ReserveEmissionMetadata` | <code>{"res_index":0, "res_type":1, "share":70}</code> | Same | Example assigns relative weight 70 to reserve 0's bToken stream. |
| `ReserveEmissionData` | <code>{"expiration":1800604800, "eps":1000000, "index":25000000000000, "last_time":1800000000}</code> | Same | V3 carry is deliberately omitted from the public view. |
| `UserEmissionData` | <code>{"index":25000000000000, "accrued":120000000}</code> | Same | V3 user carry is deliberately omitted from the public view. |
| `AuctionData` | <code>{"bid":{"G_USDC":1200000000}, "lot":{"C_BLND_USDC_LP":1000000000}, "block":1234567}</code> | <code>{"bid":{"G_USDC":1200000000}, "lot":{"C_BLNT_XLM_LP":1000000000}, "block":1234567}</code> | Shape is unchanged. These bad-debt examples show that v3 may expose the privately selected tier token in `lot`. |

## Pool factory

### Entry points

The v3 factory extends deployment with each pool's immutable backstop
configuration and exposes that configuration to the backstop and clients.

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| `__constructor(pool_init_meta)` | Same shape | Extended | V3 binds the immutable pool WASM hash, backstop, and BLNT token through `PoolInitMeta`. |
| `deploy(admin, name, salt, oracle, backstop_take_rate, max_positions, min_collateral) -> Address` | `deploy(admin, name, salt, oracle, backstop_take_rate, max_positions, min_collateral, backstop_config, access_controller) -> Address` | Extended | Deploys a pool and records its immutable tier configuration and optional controller binding. |
| `is_pool(pool_address) -> bool` | Same | Unchanged | Preserves the factory-registration boundary used by the backstop. |
| — | `backstop_config(pool_address) -> PoolBackstopConfig` | Added | Returns the registered pool's immutable ordered tiers and optional controller binding in one response. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |

### Data types

| Type | V2 example | V3 example | Comparison |
| --- | --- | --- | --- |
| `PoolInitMeta` | <code>{"pool_hash":"HASH_POOL", "backstop":"C_BACKSTOP", "blnd_id":"C_BLND"}</code> | <code>{"pool_hash":"HASH_POOL", "backstop":"C_BACKSTOP", "blnt_id":"C_BLNT"}</code> | V3 changes the field name and emission token binding to BLNT. |
| `BackstopAsset` | Not present | One of `"BlntXlm"`, `"BlntUsdc"`, `"Usdc"`, or `"Xlm"` | Canonical asset selector shared with the backstop ABI. |
| `BackstopTierConfig` | Not present | <code>{"asset":"BlntXlm", "take_rate_weight":4}</code> | One immutable loss-waterfall entry. Each weight is an independent integer from 1 through 100; no backstop oracle is configured. |
| `PoolBackstopConfig` | Not present | <code>{"access_controller":null, "tiers":[{"asset":"BlntXlm", "take_rate_weight":4}]}</code> | Factory-attested configuration consumed by the shared backstop. |

## Access controller

V3 standardizes only the read interface used by a permissioned pool and its
shared backstop. Controller deployment, administration, and permission logic
are intentionally outside the Blend ABI.

### Entry points

| V2 entry point | V3 entry point | Classification | Reason |
| --- | --- | --- | --- |
| — | `permissions(pool, user) -> u32` | Added | Returns pool-local permission bits: bit 0 permits reserve supply, bit 1 permits borrowing, and bit 2 permits backstop deposits. Higher bits are ignored. [V3 §4.7](V3_SYSTEM_SPEC.md#47-permissioned-pools--added) |
