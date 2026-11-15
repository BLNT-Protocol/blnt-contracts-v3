# Blend V2 vs. BLNT V3: Safety and Security Comparison

- **Review date:** 2026-11-03
- **Blend V2 baseline:** `ba22b487b2c5057a4ecc28b05b5193c28e4bd117`
- **BLNT V3 deployed source:** `5d055ca89afc9ed443d36b7ad34e3de59b487764` (`v3.0.2`)
- **Canonical Comet V1.1 source:** `b3a2d827ed6c158341f8e67ea91eb3b0395d3871` (`v1.1.0`)

## Executive summary

BLNT V3 has stronger defenses than Blend V2 against several concrete classes of failure: reserve-custody mismatches, fee-on-transfer or rebasing behavior, issuer deauthorization, direct custody loss, arithmetic overflow in valuation, rounding leakage, malformed future-emitter migrations, initialization front-running, and the total failure of one mandatory backstop asset. It also makes loss handling more explicit through immutable backstop tiers and a strict waterfall.

Those improvements do **not** make V3 unconditionally safer than V2. V3 is materially larger and more complex, introduces several new state machines and external dependencies, and deliberately removes V2's minimum-backstop activation threshold. A V3 pool may be active with little backstop capital or no backstop at all. Permissioned pools additionally trust an external access controller and relevant reserve issuers. Like V2, V3 derives backstop value from manipulable current Comet reserve composition rather than a TWAP or independent oracle; V3 extends that inherited exposure to a second canonical LP, cross-pool XLM pricing, emission weight, and protocol-fee auctions.

The assurance picture therefore cuts in opposite directions:

- **Design-level failure containment is generally stronger in V3.** The implementation has defenses for failure modes that V2 either did not model or could not recover from cleanly.
- **Economic protection is more configurable and less mandatory in V3.** Suppliers must evaluate each pool's tiers, funding, assets, controller, issuer powers, oracle, and administrator rather than relying on one protocol activation threshold.
- **Implementation assurance is currently stronger for V2.** V2 has published security-assessment and formal-verification reports and more production history. The V3-specific changes explicitly have not undergone an independent audit.
- **V3's Comet dependency is safer than V2's deployed Comet V1 dependency, but it is not risk-free.** V3 pins the exact official Comet V1.1 artifact and locks its controller, while V2 depended on the incident-affected Comet V1 pool. Comet V1.1 still retains the code-level extreme-supply intermediate-overflow limitation discussed in Comet PR #18, but a normally initialized pool could not realistically reach the affected LP supply through ordinary liquidity growth; the limitation became operationally relevant in the incumbent V1 pool only after the incident inflated its LP supply.
- **V3 is a better basis for safely supporting the intended new features, but it should not be represented as proven safer overall until the V3 delta and deployment configuration are independently audited.**

The concise conclusion is: **V3 improves accounting integrity, containment, and migration safety; V2 retains the advantage in simplicity, audit maturity, and mandatory minimum backstop policy.**

## Scope and method

This is a source-and-specification comparison, not an independent security audit. It covers the checked-out lending pool, backstop, pool factory, emitter, and canonical Comet dependency. This repository's deployment registries record the mainnet deployment's pinned artifacts, constructor bindings, and verification evidence. This review relies on those records but did not independently repeat a fresh on-chain verification.

The review used:

- the [V2 frozen behavioral specification](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/V2_SYSTEM_SPEC.md);
- the [V3 delta specification](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/V3_SYSTEM_SPEC.md);
- the [public API comparison](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/V2_V3_PUBLIC_API.md);
- the V2 and V3 Rust implementations and tests;
- the exact deployed-V2 artifact sizes recorded in [the V2 WASM baseline](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/wasm-size-v2-baseline.json); and
- the pinned Comet V1.1 source and official release artifact.

At the reviewed V3 commit, `make test` completed successfully. It rebuilt the four optimized contracts, verified source metadata and the pinned Comet artifact hash, passed all reported unit and integration test binaries (586 tests in total), and successfully checked the fuzz-target crate. Passing tests are evidence of intended behavior, not a substitute for an adversarial audit.

## What V3 inherits from V2

V3 is not a ground-up rewrite. Its specification says that every V2 behavior is inherited unless the V3 specification explicitly adds, replaces, extends, fixes, or exempts it. Important inherited protections include:

- isolated pool accounting;
- two-step pool-admin transfer;
- seven-day delayed reserve reconfiguration outside setup;
- immutable pool oracle, backstop, factory, and pool WASM bindings;
- no general administrator-controlled WASM replacement;
- fixed-point bToken and dToken accounting;
- conservative collateral and liability rounding;
- health, collateral, supply-cap, utilization, and position-count checks;
- atomic rollback on failed requests;
- permissionless liquidation, bad-debt, and interest-auction lifecycles;
- a 17-day queued backstop withdrawal; and
- the 70/30 emissions split between backstop and pool positions.

Both versions also retain pool-local administrators. “Immutable” does not mean that every pool parameter is frozen: a pool admin can still update allowed pool parameters, queue reserve configuration changes, set administrative statuses, and configure supply/borrow emissions. Neither version has a protocol-wide administrator that can replace contract code.

## High-level comparison

| Area | Blend V2 | BLNT V3 | Security direction |
| --- | --- | --- | --- |
| Backstop assets | One mandatory BLND:USDC Comet V1 LP | Immutable zero-to-three tiers using canonical BLNT:XLM LP, BLNT:USDC LP, USDC, or XLM | Better diversification and containment when configured well; more complexity and configuration risk |
| Minimum protection | Backstop threshold gates active status and reward-zone eligibility | No minimum backstop value; zero-tier pools allowed | Weaker mandatory supplier assurance; better liveness and operator choice |
| Bad-debt handling | One LP lot, then supplier loss | Strict first/second/third tier waterfall, then supplier loss | Stronger and more explicit containment when tiers are funded |
| Reserve custody | Accounting assumes expected token behavior | Exact transfer deltas, custody invariant, permissionless deficit reconciliation | Material V3 improvement |
| Reserve deauthorization | Not explicitly modeled | Reserve-local quarantine; liabilities stay valued and exits/recovery remain available | Material V3 improvement |
| Generic SEP-41 token risk | Trusted token behavior | Same trust remains, with observable mismatch checks | Improved detection, not eliminated trust |
| RWA controls | No standardized permission controller or accounting-aware clawback | Optional immutable controller, forced exits, accounting-aware SAC clawback | Enables RWA safely relative to naive support, but adds issuer/controller trust |
| Arithmetic | Fixed-point rules and release overflow checks | Same, plus checked I256 intermediates, explicit carries, and more bounds | Improved for V3-specific calculations |
| Emitter initialization | Public one-time initialization | Constructor-bound one-time initializer, discarded after use | Material V3 improvement |
| Future emitter migration | Caller supplies the next comparison token | Candidate attests `migration_token`; checked at queue and finalization; seven-day execution window | Material V3 improvement |
| Backstop valuation | One Comet V1 LP and threshold formula | Canonical Comet V1.1 reserve-implied values across tiers | Better validation and dependency version; more spot-price paths |
| Auctions | Types 0–2; $200 interest-lot minimum | Tier-aware types 1–2, new type 3 fee auction, $20 minimum | Better small-pool liveness; larger economic and implementation surface |
| Protocol fee | None | Immutable 0.2% of positive borrower interest, auctioned for BLNT and burned | New accounting and auction surface |
| Reward-zone gate | Backstop activation threshold | Any strictly positive eligible underlying BLNT weight | More permissionless; easier dust admission while zone is not full |
| Audit maturity | Published audit/formal-verification reports and production history | V3-specific changes not independently audited | V2 advantage |

## Where V3 materially improves security

### 1. Exact custody-delta verification

V2 updates accounting and then relies on token transfers behaving as expected. V3 snapshots the expected final balance for every touched reserve, verifies that the projected custody fully backs post-action accounting, performs the transfers, and then requires the observed balance to match exactly. The same protection applies to ordinary submissions, allowance submissions, and flash loans.

This matters because a nominally SEP-41-compatible token may charge a transfer fee, rebase during a call, transfer a different amount, or otherwise violate the assumptions used by lending accounting. In V2, a discrepancy can leave internal claims larger than actual custody. In V3, an observable mismatch fails atomically before the updated user and reserve state is committed.

This defense is not absolute. A malicious generic token that consistently lies through its `balance` entry point can still deceive the pool. V3 explicitly retains generic SEP-41 support, so token code, upgrade authority, issuer controls, and mutable dependencies remain pool-specific trust assumptions.

### 2. Explicit reserve-custody deficit reconciliation

V3 adds permissionless `reconcile_loss(asset)`. If actual reserve custody is below accrued accounting, sensitive reserve mutations remain blocked until the deficit is recognized. Reconciliation applies the loss only to the affected reserve in this order:

1. unpaid protocol credit;
2. the reserve's supplier exchange rate; and
3. unpaid backstop/take-rate credit.

It cancels affected auctions, records the recognized amounts, and is idempotent until another custody loss occurs. It does not fabricate backstop liabilities, draw an unrelated backstop tier, or contaminate another reserve.

This is an important containment improvement. A direct token clawback or custody anomaly can no longer leave a hidden deficit that later users escape or unknowingly absorb through ordinary operations. The trade-off is important: direct custody loss is **not** covered by deposited backstop capital. Suppliers of the affected reserve still bear the residual loss after protocol credit. The feature makes the loss deterministic and contained; it does not make issuer seizure harmless.

### 3. Fail-closed handling of issuer deauthorization

For Stellar Asset Contract reserves, V3 reads whether the pool remains authorized. A deauthorized reserve is quarantined without freezing unrelated reserves:

- nonzero transfer-dependent operations fail atomically;
- its collateral contributes zero borrowing capacity;
- liabilities retain full oracle value and continue accruing interest;
- liquidation and bad-debt resolution remain possible through internal position transfers;
- interest and protocol credits remain pending rather than disappearing; and
- reauthorization restores normal behavior prospectively.

This is safer than treating deauthorization as debt forgiveness, a global pool failure, or an unexplained custody deficit. It also limits blast radius to the affected reserve.

### 4. Accounting-aware RWA clawback and bounded exit mechanisms

V3 adds an accounting-aware `clawback` entry point for clawbackable SAC reserves. It requires the current SAC administrator, burns the exact pool-held asset amount, removes corresponding user bTokens (ordinary supply before collateral), checks the observed balance decrease, and invalidates an affected liquidation auction if collateral changed. Failure rolls back the entire operation.

Permissioned pools may bind one immutable external controller. Positive permissions are required for supply, collateral, borrow, flash-loan, backstop-deposit, and applicable auction-fill actions. Repayment, ordinary withdrawal, claims, and queued withdrawal do not require a positive permission, so a failed controller is not supposed to trap users in risk-increasing positions. When permissions are affirmatively absent, bounded forced-exit operations can return assets only to the user or create a caller-unparameterized borrower-exit auction.

These mechanisms are security improvements **for pools that need RWA restrictions**: they integrate revocation and issuer powers with accounting rather than pretending the powers do not exist. They are not improvements over a fully permissionless asset with no issuer control. The issuer and controller become trusted external components, and a malicious or buggy controller can force economically costly exits even though it cannot redirect user funds.

### 5. Tier-isolated backstop accounting and a strict loss waterfall

V2 has one shared backstop asset type. V3 gives each pool an immutable ordered list of zero to three independently accounted tiers. Deposits, shares, queues, emissions, take-rate allocations, interest donations, and bad-debt draws are tracked per tier. Bad debt must consume usable value from the first configured tier before the second and third, and supplier loss begins only after every configured tier is exhausted.

This improves security in three ways:

- a failure in one backstop asset need not make all forms of protection unusable;
- loss priority is disclosed and immutable rather than decided during an incident; and
- pool-attributed value and user claims remain isolated by pool and tier.

V3 also permits expired worthless shares to be burned for zero assets after a tier is fully drained. That avoids permanently trapping a drained tier in a state where worthless outstanding shares prevent a clean future initialization.

The benefit exists only when operators choose sound tiers and users fund them. Repeating USDC or XLM across multiple tiers does not diversify issuer or asset risk, and V3 imposes no protocol concentration limit or haircut.

### 6. Stronger numeric, rounding, and resource rules

V3 retains checked release arithmetic and adds checked I256 intermediates where fixed-point multiplication can exceed i128 before division. It also:

- bounds direct, allowance, and flash-loan submissions to 60 requests;
- bounds tier counts, reserve batches, queue entries, reward-zone membership, and emission configuration;
- validates exact oracle decimal scale, positive values, and inclusive validity timestamps;
- explicitly renews important instance and persistent state; and
- carries specified floor remainders forward at global, pool, tier, reserve, and user scopes.

The carry scheme is primarily a conservation and fairness improvement: V2 discards many integer remainders, while V3 retains them within a bounded scope. Checked wide intermediates and request bounds are direct safety improvements because they turn unrepresentable or resource-heavy states into atomic failures.

### 7. Safer emitter initialization and distribution coupling

The legacy emitter's first `initialize` call has no constructor-bound authority. V3 binds an initializer in the constructor, requires its authorization, verifies a seven-decimal BLNT Stellar Asset Contract administered by the emitter, and permanently removes the initializer after success. This prevents a separate first-call initialization race.

V3 also allows only the current backstop to call the emitter's `distribute`. Distribution remains permissionless through the backstop, which couples minting with the backstop's accounting checkpoint. In V2, an unrelated caller can invoke the emitter directly and advance its distribution checkpoint even though the tokens still go to the backstop. V3 removes that accounting desynchronization surface without creating a privileged keeper.

### 8. Future migration-token attestation

The V2 emitter compares candidate and incumbent balances using the incumbent token but accepts a caller-provided `new_backstop_token` for the next epoch without proving that it is the candidate's intended comparison token. A successful migration can therefore install an arbitrary next comparison token, weakening future takeover contests and potentially enabling repeated migration-drop attempts.

V3 requires every candidate to expose an immutable `migration_token()` view. The emitter checks that declaration when the queue is created and again at finalization. Cancellation also treats failed attestation as invalid. V3 adds a seven-day execution window after the inherited 31-day queue, after which anyone can cancel the stale queue and begin again.

This materially improves future-upgrade integrity. It does not eliminate economic governance through the balance contest, and the candidate contract remains part of the migration trust boundary, but it prevents token substitution through the emitter call itself.

### 9. Canonical dependency and deployment validation

V3 binds exact BLNT, USDC, XLM, BLNT:USDC LP, and BLNT:XLM LP addresses. Its backstop constructor rejects address collisions, non-seven-decimal tokens, incorrect Comet pairs, and weights other than 80:20. The build pins the official Comet V1.1 release WASM by SHA-256. The deployment specification additionally requires non-clawbackable canonical custody balances and permanently locked Comet controllers.

This is much stronger than accepting arbitrary backstop assets at runtime. Two qualifications matter:

- the backstop constructor validates interfaces, pair composition, weights, and decimals; the exact Comet WASM hash, controller lock, and SAC clawback flags are deployment-verification responsibilities rather than all being provable through the constructor; and
- the committed deployment records document the deployed WASM hashes and bindings, but independent reviewers should reproduce those read-only checks against the live deployment.

### 10. Safer Comet baseline than the deployed V2 dependency

V2's mandatory BLND:USDC backstop depends on Comet V1. The V3 canonical LPs use the exact official Comet V1.1 artifact. The V1.1 source history includes, among other changes, rejection of same-token swaps that could corrupt reserves, exact-swap corrections, vector-length validation for joins/exits, negative-amount validation, arithmetic improvements, and increased internal precision.

V3 also permits other tiers and therefore need not make every pool's loss protection depend on a single LP contract. This directly matters after the Comet V1 incident: a failure of the sole V2 backstop token impaired backstop liquidity operations and protocol liveness across V2 pools.

Comet V1.1 is not equivalent to a hypothetical fully corrected Comet V2. It does not include the intermediate-overflow correction proposed in [Comet PR #18](https://github.com/CometDEX/comet-contracts-v1/pull/18). The code-level limit still exists, but reaching it through ordinary deposits in a normally initialized pool would require an economically unrealistic amount of liquidity. The incident made it operationally relevant in the incumbent V1 pool only by inflating the LP supply to an otherwise unrealistic scale. The external [Lean analysis in PR #34](https://github.com/CometDEX/comet-contracts-v1/pull/34) is useful assurance for modeled successful liquidity-operation paths; it is not a substitute for an audit or a proof that the deployed Rust/WASM has no other defects.

## Where V3 expands the attack surface or weakens mandatory guarantees

### 1. Much more code and state

The most objective attack-surface increase is size and complexity. Using the checked-in deployed-V2 sizes and a clean V3 build at the reviewed commit:

| Contract | Deployed V2 | Reviewed V3 | Change |
| --- | ---: | ---: | ---: |
| Backstop | 30,772 B | 47,682 B | +55.0% |
| Emitter | 10,448 B | 7,203 B | -31.1% |
| Pool | 57,328 B | 81,376 B | +41.9% |
| Pool factory | 3,230 B | 6,226 B | +92.8% |
| **Total** | **101,778 B** | **142,487 B** | **+40.0%** |

WASM size is not a vulnerability metric, and some V3 code exists specifically to enforce stronger invariants. It is nevertheless a useful review proxy. V3 adds tier accounting, tier auctions, protocol-fee accounting, reserve reconciliation, clawback, permission controllers, forced exits, migration attestation, and substantially more emissions carry state. There are more interleavings and more storage invariants to audit.

### 2. No minimum backstop requirement

V2 forces pools on-ice when its backstop threshold is not met. V3 removes the minimum-value test from pool activation and allows immutable zero-tier pools. A zero-tier pool sends bad debt directly to suppliers. A minimally funded tiered pool may remain active even when its backstop would cover only a negligible loss.

This is an intentional design choice, not an implementation defect. It prevents one backstop-token failure from automatically disabling borrowing and lets sophisticated users choose unbackstopped credit markets. It also removes a protocol-enforced assurance previously given to suppliers. UI labeling, risk disclosures, and user due diligence become part of the practical security model.

### 3. Easier reward-zone entry

V2 ties reward-zone eligibility to its activation threshold. V3 requires only strictly positive eligible underlying BLNT in a canonical LP tier. While the zone has open slots, a very small qualifying balance can enter. Once full, replacement remains strict and based on greater underlying BLNT.

This does not create lending bad debt, but it enlarges emissions-gaming and low-value-pool spam exposure. The bounded 30-pool list and strict replacement rule limit resource growth and displacement, but they do not recreate V2's economic threshold.

### 4. Multiple spot-reserve pricing paths

V3 does not use an independent oracle or TWAP for backstop value. BLNT:USDC reserve composition anchors BLNT in USDC; BLNT:XLM and plain XLM values are cross-priced through the two canonical pools. Protocol-fee auctions also use the current BLNT:USDC reserve-implied price.

Swaps, one-sided liquidity changes, and donations can alter these values. That can affect:

- Q4W status calculations;
- take-rate allocation;
- bad-debt and interest-auction sizing;
- protocol-fee auction bids;
- reward-zone weights; and
- emission allocation at a checkpoint.

V3 consistently validates positive reserves, exact weights, supply, and same-invocation snapshots, so malformed data fails closed. It does not eliminate economic manipulation of valid current reserves. Two canonical pools and cross-pricing add manipulation paths relative to V2's one LP, even though V2's threshold and LP valuation were also composition-dependent.

### 5. External access-controller trust

Permissioned V3 pools call an external controller whose construction, administration, credential rules, upgradeability, and storage are outside the BLNT ABI. A controller can deny new operations and, through affirmative absence, permit forced exits. A failure or malformed response blocks operations that require positive permission and also blocks proof-of-absence forced exits.

V3 minimizes custody risk by making the controller immutable per pool, checking the position owner rather than `spender` or `to`, keeping ordinary risk-reducing exits independent of the controller, and ensuring forced withdrawals pay only the user. Even so, a controller compromise can create denial of service or forced economic exits. Every permissioned pool therefore has a larger trust boundary than a V2 permissionless pool.

### 6. Issuer and clawback trust

RWA support exposes powers that do not exist for native XLM and may not exist for non-clawbackable assets. The accounting-aware clawback path is carefully bounded, but a SAC administrator can invoke the SAC directly and bypass the pool entry point. Reconciliation then socializes the deficit according to the V3 reserve-loss order.

This is a necessary recognition of Stellar issuer powers, not a way to remove them. Users must treat issuer authorization, clawback, freeze, and administrator controls as security dependencies. A pool that accepts a generic upgradeable token has still broader token-level risk.

### 7. More auction types and economic state

V3 makes interest and bad-debt auctions tier-aware and adds an independent type-3 protocol-fee auction. Type 2 and type 3 may coexist. Private tier metadata, per-tier pending credit, cursors, partial fills, deauthorization cancellation, stale deletion, and deficit-driven cancellation all have to stay synchronized with the public V2-compatible auction projection.

The reduction of the interest-auction minimum from $200 to $20 improves liveness for smaller pools and tiers, but permits more frequent small auctions. That increases keeper, filler, fee, and timing exposure. The new protocol-fee auction also makes current BLNT spot manipulation economically relevant to another flow of reserve assets.

### 8. Larger emissions and migration state machine

V3 carries rounding remainders at multiple scopes, supports two emission-eligible LP types, compounds claims into the originating LP, has initial-launch and future-migration drop rules, and retains future emitter replacement. These changes improve conservation and migration safety, but add more storage entries, lifecycle transitions, and invariant relationships.

The initial emitter may make a one-time deployment-selected drop of up to 150 million BLNT. Future migration drops retain the 50-million combined ceiling, with at most 40 million in the immutable list and up to 10 million in backfill. The larger initial mint authority is intentionally one-time and protected by constructor-bound initialization, but a deployment configuration error has a larger monetary impact than V2's 50-million migration ceiling.

### 9. Operational liveness matters more

Most V3 maintenance remains permissionless, which is a strong design choice. In practice, keepers and auction bots are still needed for timely distribution, emission gulps, oracle refresh in applicable deployments, auction creation, filling, stale deletion, and health monitoring. More tiers and auction types mean more opportunities for delayed maintenance or poor economic execution.

Delayed calls generally do not grant a keeper custody, but they can delay emissions, allow credits to accumulate, leave stale auctions unresolved, or postpone loss recognition. Operational dashboards and independent callers are therefore security-relevant even though they are not privileged protocol components.

### 10. Immutability increases the cost of mistakes

V3 deliberately has no emergency override, protocol-wide governance, privileged WASM replacement, or alternate upgrade path. This reduces administrator-takeover risk. It also means a critical V3 defect cannot be patched in place. Recovery requires user workarounds, a future version, and—when the emitter remains trustworthy—the normal emitter/backstop migration mechanism. The [immutable deployment security response plan](IMMUTABLE_SECURITY_RESPONSE.md) documents this constraint and the response sequence.

That trade-off is acceptable only if the immutable code and exact deployment configuration receive a level of review commensurate with the larger feature set.

## Known V3 compatibility limitation: `Repay` with `i128::MAX`

The reviewed V3 code has a concrete compatibility regression relative to V2. A `Repay` request using `amount: i128::MAX`—a common “repay everything” sentinel—fails with `OverflowError` when the custody snapshot computes:

```text
balance_before + tokens_in - tokens_out
```

For an over-repayment, action construction records the full requested amount as `tokens_in` and the excess as `tokens_out`. Although the net transfer is bounded by live debt, adding `i128::MAX` to an already positive pool balance overflows before the subtraction occurs.

The practical workaround is to use a bounded maximum repayment amount. The contract then applies its ordinary over-repayment netting/refund behavior, so the user need not know the debt to the final base unit. The failure is atomic and does not appear to let an attacker extract value; its impact is integration incompatibility and denial of the sentinel-based request. It affects plain submissions and the request vector used with flash loans because both use the same custody snapshot.

This issue illustrates V3's central trade-off: the exact-balance invariant is a real improvement, but its new intermediate arithmetic creates a failure mode that V2 did not have.

## Threat-by-threat assessment

| Threat or failure mode | Relative assessment | Reason |
| --- | --- | --- |
| Unauthorized protocol code replacement | Roughly equal / low | Neither version exposes a general protocol WASM upgrade path |
| Pool-admin abuse | Roughly equal | V3 inherits V2 pool-local admin powers and delayed reserve changes |
| Initialization front-running | V3 stronger | Constructor-bound initializer is required and then discarded |
| Emitter comparison-token substitution | V3 stronger | Candidate token attestation is checked at queue and finalization |
| Fee-on-transfer/rebase mismatch | V3 stronger | Exact before/after custody checks fail atomically |
| Direct reserve custody loss | V3 stronger containment | Permissionless, reserve-local reconciliation prevents hidden deficits |
| Issuer deauthorization | V3 stronger containment | Affected reserve is quarantined while debt remains enforceable |
| Malicious generic SEP-41 token | Still high-risk in both | V3 detects observable mismatches but trusts reported balances and token behavior |
| Backstop-asset failure | V3 potentially stronger | Multiple tiers and strict waterfall avoid one mandatory asset, if configured and funded |
| Insufficient backstop capitalization | V2 policy stronger | V3 deliberately has no minimum and permits zero-tier pools |
| Rounding leakage | V3 stronger | Scoped carries conserve specified remainders |
| Arithmetic overflow | V3 generally stronger, with a regression | I256/checks improve valuation; `i128::MAX` repay snapshot can overflow |
| Oracle failure | Similar core dependency | Lending still trusts each pool's immutable SEP-40 oracle; V3 validates scale and inclusive expiry more strictly |
| Backstop price manipulation | Mixed | V3 validates canonical pools and snapshots, but adds cross-pool and protocol-fee spot-price uses |
| State/resource exhaustion | V3 stronger bounds | Requests and new vectors/batches are explicitly bounded |
| Controller compromise | V3-only risk | Applies only to permissioned pools; exits are designed to limit custody impact |
| Auction/keeper liveness | Mixed | Lower threshold and permissionless recovery help; more auction paths require operation |
| Audit and battle-tested maturity | V2 stronger | V2 has published reviews and longer operational history; V3 delta is unaudited |

## What suppliers and borrowers should infer

### Suppliers

V3 gives suppliers better visibility into, and containment of, many technical failure modes. It does **not** promise a funded backstop. Suppliers should inspect:

- whether the pool has zero, one, two, or three tiers;
- the current transferable value in each tier, not only configured asset names;
- waterfall order and take-rate weights;
- concentration in one issuer or LP;
- reserve token implementation and upgrade/issuer powers;
- access-controller code and administration;
- pool oracle quality and freshness;
- pool-admin identity and mutable parameters; and
- keeper and auction health.

An “Active” V3 pool is not a statement that a minimum amount of loss protection exists.

### Borrowers

Borrowers retain V2's health, collateral, liquidation, and interest model. In a permissioned pool they additionally face controller revocation and a forced-exit auction. A reserve issuer may deauthorize or claw back an asset; liabilities remain due and collateral may lose borrowing value immediately. Risk-reducing actions are intentionally kept available, but market liquidity and token authorization can still make a theoretical exit difficult.

### Backstop depositors

V3 depositors know their immutable tier and waterfall priority, but queued shares remain exposed to loss as in V2. Different tiers can have different asset, issuer, liquidity, price-manipulation, and emission characteristics. Plain USDC or XLM tiers do not earn BLNT emissions. Only canonical BLNT LP tiers do.

## Assurance and release recommendations

Before treating V3 as having a stronger overall security posture than V2, the following work would provide the most value:

1. **Commission an independent audit of the entire V3 delta**, not only new public entry points. The audit should cover cross-contract invariants among the pool, backstop, factory, emitter, Comet pools, SACs, and optional controller.
2. **Prioritize the highest-complexity state machines:** tier bad-debt and interest auctions, protocol-fee auctions, custody reconciliation, zero-supply debt behavior, forced exits, emissions carries, and future emitter migration.
3. **Enforce bounded repay-all in integrations.** The V3 specification documents the unsupported `i128::MAX` sentinel, and the contract suite covers finite bounded repayment through allowance and flash-loan paths. Maintained SDK and UI integrations should preserve that rule.
4. **Independently reproduce deployment verification.** The committed [core registry](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/deployments/mainnet-v3-core.json) and [Fixed-pool registry](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/deployments/mainnet-v3-fixed-pool.json) provide project verification evidence; independent reviewers should rerun read-only checks against the live contracts and compare their artifacts and bindings with the pinned deployment inputs.
5. **Audit every production access controller and material generic SEP-41 reserve separately.** The core protocol cannot compensate for a controller or token that lies or changes behavior maliciously.
6. **Monitor economic manipulation and liveness.** Track canonical LP depth/composition, cross-pool price divergence, backstop value per tier, queue ratios, pending credits, active/stale auctions, reconciliation events, reserve authorization, oracle age, emitter checkpoints, TTL horizons, and keeper success.
7. **Stress test spot manipulation around one transaction.** Simulate swaps/donations before reward-zone changes, bad-debt creation, interest-auction creation, and protocol-fee creation, including flash-liquidity assumptions.
8. **Property-test conservation across partial fills and cancellation.** For each tier and reserve, verify that deposits, shares, queued shares, pending credit, auction lots, donations, draws, carries, and claims neither create nor strand value.
9. **Maintain the immutable response plan.** Keep the documented [response process for any future critical security finding](IMMUTABLE_SECURITY_RESPONSE.md), including preparation and normal migration of a V4 candidate when necessary, aligned with the deployed migration semantics. Rehearse it periodically rather than assuming an emergency patch is possible.

## Final assessment

BLNT V3 is more security-conscious than V2 in its treatment of real-world failure modes. Its strongest improvements are exact custody verification, explicit reserve-loss reconciliation, fail-closed authorization handling, immutable tiered loss containment, safer emitter lifecycle checks, and canonical dependency validation. These changes matter because they prevent silent accounting divergence, limit cross-reserve contagion, preserve liabilities during issuer failures, and close a concrete future-migration token-substitution weakness.

V3 simultaneously moves from one relatively simple backstop model to a configurable credit-protection and RWA framework. It adds external controller trust, issuer-aware exits, two canonical LP dependencies, current-reserve cross-pricing, tier-aware auctions, a protocol fee and burn auction, substantially more emissions accounting, and zero-backstop operation. It also removes V2's mandatory economic threshold. Those are legitimate product choices, but each increases the amount of code, configuration, and operational behavior that must be correct.

Accordingly, the fair characterization is not “V3 is safer than V2” without qualification. It is:

> **V3 has stronger safety mechanisms and better containment for known failure classes, while V2 has a smaller, independently reviewed, more battle-tested implementation and a stronger mandatory backstop-capital policy. V3's net security advantage depends on independent review and disciplined pool and deployment configuration.**

## Source references

- [BLNT V3 specification index](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/SYSTEM_SPEC.md)
- [Frozen Blend V2 behavior](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/V2_SYSTEM_SPEC.md)
- [BLNT V3 changes](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/V3_SYSTEM_SPEC.md)
- [V2/V3 public API comparison](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/docs/V2_V3_PUBLIC_API.md)
- [V3 exact custody snapshots](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/pool/src/pool/submit.rs)
- [V3 reserve custody and reconciliation](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/pool/src/pool/gulp.rs)
- [V3 reserve clawback](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/pool/src/pool/clawback.rs)
- [V3 permission checks](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/pool/src/access.rs)
- [V3 forced exits](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/pool/src/pool/permissioned.rs)
- [V3 emitter migration checks](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/emitter/src/backstop_manager.rs)
- [V3 build profile](https://github.com/BLNT-Protocol/blnt-contracts-v3/blob/main/Cargo.toml)
- [Blend V2 audit directory](https://github.com/blend-capital/blend-contracts-v2/tree/ba22b487b2c5057a4ecc28b05b5193c28e4bd117/audits)
- [Official pinned Comet V1.1 source](https://github.com/CometDEX/comet-contracts-v1/tree/b3a2d827ed6c158341f8e67ea91eb3b0395d3871)
- [Comet V1.1 release](https://github.com/CometDEX/comet-contracts-v1/releases/tag/v1.1.0)
- [Comet intermediate-overflow proposal, PR #18](https://github.com/CometDEX/comet-contracts-v1/pull/18)
- [Comet Lean analysis, PR #34](https://github.com/CometDEX/comet-contracts-v1/pull/34)
