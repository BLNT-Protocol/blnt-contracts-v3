# Blend V3 Contract Specification

Status: Draft 0.2

## 1. Purpose and inheritance

This document defines only v3 differences. Unstated behavior inherits
[V2_SYSTEM_SPEC.md](V2_SYSTEM_SPEC.md) under [SYSTEM_SPEC.md](SYSTEM_SPEC.md).
A difference MUST be classified here as an addition, replacement, extension,
safety fix, or approved exception.

Scoped integer carry-forward is an approved conservation fix; Section 4's
12,500-USDC threshold is the sole approved economic exception. V3 adds no
protocol fee, protocol-wide admin, multisig, governance, emergency override,
privileged WASM replacement, or alternate upgrade path. Deployment grants no
continuing authority.

Normative terms `MUST`, `MUST NOT`, `SHOULD`, and `MAY` describe requirements.

### 1.1 Scoped carry

Every calculation that invokes this subsection conserves its floor remainder
at the stated scope. For amount \(X\), prior carry \(C\), nonnegative weights
\(w_i\), and \(W=\sum_i w_i>0\), proportional allocation means:

\[
A_i=\left\lfloor\frac{(X+C)w_i}{W}\right\rfloor,
\qquad C' = X+C-\sum_i A_i.
\]

A scalar division by positive \(D\) analogously returns
\(q=\lfloor(N+C)/D\rfloor\) and \(C'=N+C-qD\). Each carry MUST remain
nonnegative, in its original units, and available only to the next operation
at the same global, pool, pool-tier, reserve-tier, or user scope.

## 2. Runtime — **Replaced**

`V2-RUNTIME-001` is replaced: V3 MUST target Stellar Protocol 27, Rust 1.91.1,
Soroban SDK 27.0.3, and `wasm32v1-none`.

## 3. Multi-asset backstop — **Replaced and extended**

This section replaces `V2-BACKSTOP-001`'s single token and extends
`V2-BACKSTOP-002` and `V2-BACKSTOP-003` per tier.

### 3.1 Asset configuration

At deployment, each pool operator supplies an ordered list of one to three
`BackstopTierConfig` values. Entry order maps to `FirstLoss`, `SecondLoss`, and
`ThirdLoss`; omitted trailing positions do not exist. Each entry selects one
unique asset from canonical BLNT:XLM LP, BLNT:USDC LP, USDC, and XLM and one
integer take-rate weight from 1 through 100. Weights are independent of loss-
waterfall order. The factory stores this immutable configuration with the pool
registration, and the backstop verifies and caches it before accepting the pool.

The candidate also immutably binds the canonical BLNT:USDC and BLNT:XLM 80:20
Comets and canonical BLNT, USDC, and XLM assets. No other backstop asset is
accepted and no backstop tier has an oracle configuration. Each canonical
Comet MUST be initialized unfrozen and then bound to a classic-account
controller whose signer weights are all zero and whose authorization
thresholds are positive. Deployment verification MUST reject any other
controller state. This makes the Comet's controller-only freeze and controller-
replacement operations permanently unreachable.

Every transferable tier counts equally per verified USDC for activation,
participates in take-rate allocation using its configured weight, and absorbs
loss in configured order without a protocol-level haircut or concentration
limit. A deauthorized plain-USDC tier has zero transferable value under
Section 4 until the issuer reauthorizes the backstop balance.
Only exact canonical BLNT:USDC and BLNT:XLM Comet tiers are BLNT-emission
eligible. A pool with neither receives no BLNT emissions. Plain-USDC and
plain-XLM interest proceeds have the buy-and-burn haircut in Section 5.3.

### 3.2 Position accounting

Each configured pool-tier independently applies `V2-BACKSTOP-001` and
`V2-BACKSTOP-002`; every deposit additionally verifies the pool through the
immutable factory before custody changes. Public share operations and
pool-authorized `donate` select a configured `BackstopTier`. The consolidated
`pool_data` returns an ordered tier vector containing asset identity, token,
configured weight, emission eligibility, tokens, shares, and USDC-equivalent
value, plus aggregate active value and value-weighted Q4W. Active BLNT, queued
value, emission data, and migration data remain internal.

As a v3 liveness safety fix, expired shares in a fully drained tier MAY be
burned for zero assets. A new deposit remains prohibited while worthless
shares exist and may initialize a fresh one-to-one exchange rate only after all
outstanding shares are removed. No administrator or external caller can adjust
an exchange rate directly.

### 3.3 Withdrawals

Each tier independently applies `V2-BACKSTOP-003`, except that its 20-entry
limit is aggregate per user and pool across all configured tier queues.

Capital state has these canonical policy effects:

| Capital state | Activation/status | Loss | Take rate | Ongoing BLNT |
| --- | --- | --- | --- | --- |
| Active accounted shares | Included | Available | Included | BLNT-bearing tiers |
| Queued shares | Excluded | Available | Included | Excluded |
| Pool-selected bad-debt lot | Included until drawn | Selected for that auction | Included | Inherits active-share state until drawn |
| Raw direct transfer | Excluded | Excluded | Excluded | Excluded |
| Deauthorized plain-USDC shares | Excluded until reauthorized | Skipped | Excluded | Ineligible |

Prepared, partially filled, stale, and continued bad-debt auctions MUST retain
the corresponding v2 withdrawal-blocking liability until settlement or
supplier default clears it atomically.

### 3.4 Non-clawbackable custody — **Safety requirement**

The backstop exposes no issuer-clawback entry point. A deployment MUST prove
that every relevant issuer-controlled contract balance was created
non-clawbackable before accepting the deployment:

- the shared backstop's plain-USDC balance;
- each BLNT and paired-asset SAC balance held by the canonical Comets; and
- any issued-XLM backstop balance used by a non-production fixture.

Native XLM has no issuer clawback, and the canonical Comet LP contracts expose
no issuer-clawback operation. Because a SAC does not expose its stored contract-
balance clawback flag through a public method, deployment verification MUST
read the exact persistent balance entry and fail closed unless it exists, is
authorized, has positive custody, and records `clawback = false`. A failed
check invalidates the deployment. This requirement does not prevent later SAC
deauthorization, which remains subject to Section 4's accepted behavior.

## 4. Pool activation — **Replaced**

This replaces `V2-BACKSTOP-004`. For configured tiers (i=1\ldots n), in
seven-decimal USDC units:

\[
E_p=\sum_{i=1}^{n}V_{p,i},\qquad 1\le n\le3.
\]

Each \(V_{p,i}\) is eligible, transferable pool-attributed value. Every
verified transferable USDC has equal weight, so any combination—including one
asset alone—may qualify.

\[
T_{\mathrm{activation}} = 12{,}500\ \mathrm{USDC}
\]

Equality qualifies. Falling below the threshold deactivates a pool, and
reactivation uses the same threshold.

The backstop immutably binds distinct BLNT, USDC, and XLM tokens and the exact
BLNT:USDC and BLNT:XLM Comets. All five token interfaces MUST use seven
decimals. Construction rejects a Comet unless it contains exactly its expected
pair at normalized weights 80% BLNT and 20% paired asset.

Let the BLNT:USDC Comet hold BLNT reserve \(B_u\), USDC reserve \(U\), and LP
supply \(S_u\). Its current reserve composition implies total USDC value
\(T_u=5U\). For BLNT:USDC LP amount \(A_u\):

\[
V_u(A_u)=\left\lfloor A_u\frac{5U}{S_u}\right\rfloor
\]

The same Comet implies a BLNT price of \(4U/B_u\). Let the BLNT:XLM Comet hold
BLNT reserve \(B_x\), XLM reserve \(X\), and LP supply \(S_x\). Its implied
total USDC value and the value of BLNT:XLM LP amount \(A_x\) are:

\[
T_x=\left\lfloor\frac{5B_xU}{B_u}\right\rfloor,
\qquad
V_x(A_x)=\left\lfloor A_x\frac{T_x}{S_x}\right\rfloor
\]

For either Comet, underlying BLNT is
\(B(A)=\lfloor AR_b/S\rfloor\). Every quote rechecks positive LP supply and
reserves and the immutable weights. Canonical USDC is valued one-for-one only
while the USDC SAC reports the backstop contract as authorized. A deauthorized
plain-USDC tier has zero value without changing its token, share, queue, or
pending-interest accounting. The same two Comets imply the USDC-equivalent
value of plain-XLM amount \(A\):

\[
V_{XLM}(A)=\left\lfloor
A\frac{UB_x}{B_uX}
\right\rfloor.
\]

Active and queued amounts are quoted separately. A zero amount has zero value
without reading an otherwise unnecessary Comet. No backstop valuation uses a
pool oracle, separate oracle, or caller-supplied price.

Each operation MUST obtain all backstop values it consumes from one canonical
snapshot. A verified zero is the only skippable result; unavailable,
incompatible, negative, inconsistent, or overflowing inputs fail atomically.

Canonical LP values deliberately reflect current Comet composition rather
than an external fair-market price. Swaps, one-sided liquidity changes, and
donations can change them; BLNT:USDC remains the USDC and BLNT-price anchor.
Plain-USDC authorization is read directly from its SAC on every valuation. A
deauthorized balance therefore contributes zero to activation, pool status,
reward-zone qualification, take-rate allocation, auction sizing, and supplier-
loss eligibility. Reauthorization restores its value prospectively; it does
not reallocate take rate or losses processed while the tier was unavailable.

The consumers are activation, status, reward-zone membership, take-rate
allocation, auction sizing, and supplier-loss eligibility. Emission weight
instead recognizes only canonical BLNT LPs and uses their same-invocation
underlying-BLNT composition under Section 6.2.

### 4.1 Pool-status valuation — **Extended**

`V2-POOL-STATUS-001` and `V2-POOL-STATUS-002` apply with \(Q_p\) replaced by
the verified-value ratio:

\[
Q_p =
\left\lceil
\frac{V_{p,\mathrm{queued}}}
{V_{p,\mathrm{active}} + V_{p,\mathrm{queued}}}
10^7
\right\rceil
\]

Active and queued values use the same canonical inputs without take-rate
weights. A zero denominator gives zero; division rounds upward at seven
decimals. Inherited thresholds and admin overrides apply to this ratio.
Statuses 0 and 1 also require Section 4's activation threshold. Queueing does
not refresh status, stored status is not an additional reward-zone or
backstop-auction gate, and each status decision uses one `pool_data` snapshot.

### 4.2 Pool integration — **Safety extensions**

`V2-POOL-ADMIN-001`, `V2-POOL-CONFIG-001`, and `V2-LENDING-001` through
`V2-LENDING-005` apply except for these extensions:

- Direct, allowance, and flash-loan submissions contain at most 60 requests.
- Oracle prices require the exact decimal scale, a positive value, and an
  inclusive unexpired validity timestamp.
- Intermediate fixed-point multiplication uses checked I256 arithmetic.
  Results that do not fit i128 fail the transaction.
- Every submission verifies exact per-asset token-balance deltas; transfer
  fees, in-call rebases, or other mismatches fail atomically.
- Backstop borrow and repay synchronize the reserve-keyed withdrawal-gate
  liability without summing unlike asset units.
- Reserve and user state use bounded persistent entries with explicit TTL
  renewal.
- Auction creation, lookup, and stale deletion retain the v2
  `new_auction`, `get_auction`, and `del_auction` entry points and auction-type
  discriminator. All three fill types use the inherited `submit` request
  discriminants; tier selection and settlement metadata remain private.
  Vector semantics for the two extended auction types are:
  - For bad debt, `bid = []` or `lot = []` means no caller assertion. A nonempty
    vector MUST exactly match the canonical debt-asset or selected-tier-token
    set, respectively.
  - For interest, `bid = []` means no caller assertion and a nonempty `bid` MUST
    contain exactly the selected tier token. `lot` remains a required, nonempty
    reserve-asset input; `lot = []` is invalid.
  Assertions are order-independent and MUST NOT influence selection, amounts,
  or pricing. A mismatch fails atomically.

### 4.3 Reserve authorization quarantine — **Safety extension**

For every configured Stellar Asset Contract reserve, the pool MUST read the
token's current authorization for the pool contract. A false authorization
quarantines only that reserve; the pool and its other reserves remain
operational. A generic SEP-41 reserve that does not expose the SAC-only
authorization extension retains the inherited v2 behavior.

While quarantined:

- Any supply, collateral supply, withdrawal, collateral withdrawal, borrowing,
  repayment, or flash-loan operation that requires a nonzero reserve-token
  transfer MUST fail atomically through the token contract, leaving no custody
  or accounting changes. An allowance-based submission MAY execute an
  accounting-only batch whose incoming and outgoing reserve transfers net to
  zero; ordinary utilization, health, and reconciliation checks still apply.
- Existing liabilities retain their complete oracle value and continue
  accruing ordinary interest. Existing supplier claims and the take-rate share
  continue accruing, although neither is transferable until reauthorization.
- The reserve contributes zero effective collateral to position health and
  borrowing capacity. Its nominal bToken value remains available to
  liquidation sizing, and its bTokens and dTokens remain transferable through
  the inherited internal user-liquidation fill.
- Deauthorized reserve credit MUST be omitted from new interest-auction lots
  without being reduced, reweighted, or written off. An active interest
  auction containing that reserve may be deleted immediately through the
  inherited permissionless deletion entry point; its unfilled credit remains
  pending.
- Existing liabilities remain eligible for ordinary user-liquidation,
  bad-debt handoff, tier auctions, and supplier default. Deauthorization alone
  MUST NOT forgive a liability, manufacture bad debt, or invoke
  `reconcile_loss` because custody remains present.

Reauthorization restores ordinary transfer and collateral behavior
prospectively and makes all retained reserve credit available to a later
interest auction. No administrator checkpoint, reserve rewrite, or pool-wide
status transition is required.

### 4.4 User-liquidation handoff — **Safety extension**

`V2-AUCTION-002` applies. Its completed-liquidation handoff additionally MUST:

- Move every residual dToken share from the liquidatee to the configured
  backstop without changing reserve dToken supply.
- Accumulate each amount with any liability the backstop already holds in that
  reserve.
- Update the configured backstop's canonical v2-style liability map.
- Clear the liquidatee's residual liabilities.
- Emit one handoff event per affected reserve.

The handoff does not modify or create a bad-debt auction; the liabilities are
input to a later permissionless type-1 `new_auction` call.

An incomplete fill never hands off. It MUST fail if it exhausts collateral or
leaves collateral and liabilities while reducing health.

The handoff uses one liability-map load and store, remains bounded by 30
reserves, and MUST fit Protocol-27 invocation limits.

### 4.5 Reserve clawback — **Added**

The pool exposes `clawback(asset, from, amount)` as a multi-reserve extension
of the Stellar Asset Contract clawback operation. It accepts a configured
reserve, a Blend position owner, and a strictly positive exact underlying
amount. `from` identifies the internal Blend position; the pool contract is
the token holder passed to the reserve SAC.

The operation MUST:

- Reject the pool itself as `from`.
- Accrue the reserve and affected user emission state before changing shares.
- Convert `amount` to bTokens by rounding upward, require the user to own that
  many supply-plus-collateral bTokens, consume ordinary supply first, and then
  consume collateral.
- Require at least `amount` of liquid reserve tokens in the pool.
- Require the current reserve SAC administrator at the pool entrypoint, then
  invoke the SAC's `clawback(pool, amount)`. The operation MUST fail unless the
  pool's SAC balance entry is clawbackable.
- Verify an exact `amount` decrease in the pool's reserve-token balance and
  emit the underlying amount and ordinary-supply and collateral bTokens
  burned.
- Preserve an active user-liquidation auction when only ordinary supply is
  consumed. If any collateral is consumed, atomically delete the auction and
  emit the inherited auction-deletion event before the clawback event so a
  fresh liquidation can be created from the updated position.

The operation is exact rather than best-effort: it does not cap to available
user shares or pool liquidity. Any failure rolls back the SAC burn, share and
reserve accounting, emissions, auction invalidation, and events atomically. It
requires no position-owner authorization, applies no pool-status or
reserve-enabled gate, and does not perform a post-clawback health check. A
resulting unhealthy position uses the inherited user-liquidation, bad-debt
handoff, waterfall, and supplier-loss paths; clawback itself neither creates
liabilities nor starts an auction.
This entry point cannot prevent the SAC administrator from invoking the SAC
directly; a direct clawback bypasses Blend position accounting.

### 4.6 Reserve-loss reconciliation — **Safety extension**

The pool exposes permissionless `reconcile_loss(asset) -> i128` for a
configured reserve whose actual token custody has fallen below its accrued
accounting balance, including after a reserve SAC administrator claws back
tokens directly from the pool. It returns the recognized underlying deficit,
or zero when custody has no deficit.

Reconciliation MUST:

- Accrue the reserve before comparing its actual token balance with
  `total_supply + backstop_credit - total_liabilities`.
- Ignore a zero or positive balance delta; positive deltas remain available to
  the inherited `gulp` operation.
- Apply the deficit directly to that reserve's suppliers using the inherited
  ceiling-rounded `b_rate` loss calculation, leaving every user's bToken
  balance and every borrower liability unchanged.
- Cap the supplier attribution at the reserve's aggregate supplier claim and
  set `b_rate` exactly to zero when that claim is exhausted. Any remaining
  deficit MUST reduce only that reserve's unpaid `backstop_credit`. It MUST
  proportionally reduce the reserve's pending tier allocations and carry, and
  MUST cancel an active interest auction whose lot contains the affected
  reserve before committing that reduction. Reconciliation MUST NOT create
  backstop liabilities, draw deposited backstop capital, or change another
  reserve.
- Emit the recognized deficit, supplier attribution, backstop-credit
  attribution, and applied `b_rate` reduction. Cancellation of an affected
  interest auction emits the inherited deletion event first. Repeated
  reconciliation without another custody loss MUST return zero and make no
  further accounting reduction.

Any reserve mutation that could let an existing or new position escape or
absorb an unreconciled deficit MUST fail until `reconcile_loss` succeeds. A
supplier-rate reduction can make borrowers using that reserve as collateral
unhealthy; those users then use the ordinary liquidation path. Only residual
borrower bad debt after collateral liquidation reaches the configured
backstop waterfall and, if necessary, the inherited supplier-default path.
Reconciliation itself has no user parameter because a direct pool-custody
clawback does not identify which supplier funded the removed tokens.

As a v3 safety fix, the minimum operational `b_rate` is 0.1 at the inherited
12-decimal rate scale. Below that threshold the affected reserve MUST reject
new supply, collateral supply, borrowing, and flash loans, while repayment,
withdrawal, liquidation, bad-debt resolution, interest realization, and
reconciliation remain available under their inherited checks. Equality is
operational. While bTokens remain, the reserve reopens automatically if
accrued borrower interest restores `b_rate` to at least 0.1. Ordinary
underlying withdrawals cannot
convert a positive asset amount at a zero exchange rate. Existing bTokens are
not burned or reset merely because the rate falls below the operational
threshold or reaches zero. Before an insolvent user's remaining
liabilities are handed to the backstop, collateral bTokens with zero current
underlying value MUST be checkpointed, forfeited, and removed. No underlying
assets are transferred. This prevents an empty-value entry from blocking the
handoff or later recovering value for a borrower whose debt was socialized.
Supply shares held by users without bad debt remain unaffected and retain any
later rate recovery.

If forfeiture removes the final bToken, `b_rate` remains zero and the reserve
cannot accept new risk. Existing liabilities remain repayable or liquidatable
and MUST continue accruing under the ordinary interest curve at 100%
utilization. Because no supplier denominator remains, the entire resulting
interest accrual MUST increase `backstop_credit`, independently of the ordinary
take-rate split. Existing take-rate credit remains realizable. Positive custody
surplus with zero supply is also added to `backstop_credit` by `gulp`. Removing
an individually zero-valued collateral position can reduce the aggregate
supplier claim by at most one underlying base unit because of fixed-point
rounding; any resulting positive custody dust follows that `gulp` rule.
An ordinary empty reserve with `b_supply == 0` and `b_rate` at or above 0.1 is
not impaired and MAY accept new supply under the inherited rules.

### 4.7 Permissioned pools — **Added**

At deployment a pool MAY immutably bind one external access-controller
address. Omitting the address creates a permissionless pool and preserves the
inherited action surface. The pool admin MUST NOT replace, remove, or add a
controller after deployment. The factory MUST record the same binding used by
the pool and return it with the pool's tier configuration so the shared
backstop applies the identical policy.

Blend standardizes only this controller entry point:

```text
permissions(pool, user) -> u32
```

Bits 0, 1, and 2 respectively mean `RESERVE_SUPPLY_ALLOWED`,
`RESERVE_BORROW_ALLOWED`, and `BACKSTOP_DEPOSIT_ALLOWED`. Blend ignores higher
bits. Controller construction, pool registration, permission administration,
credential evaluation, code mutability, and every other controller entry
point are implementation decisions outside the Blend ABI. The controller
address and the trust implied by its implementation MUST be disclosed to pool
users.

For a permissioned pool, the position owner MUST have:

| Operation | Required bits |
| --- | --- |
| Reserve supply | `RESERVE_SUPPLY_ALLOWED` |
| Collateral supply | `RESERVE_SUPPLY_ALLOWED` and `RESERVE_BORROW_ALLOWED` |
| Borrow or flash loan | `RESERVE_BORROW_ALLOWED` |
| Backstop deposit or dequeue | `BACKSTOP_DEPOSIT_ALLOWED` |
| Fill a user-liquidation or borrower-exit auction | `RESERVE_SUPPLY_ALLOWED` and `RESERVE_BORROW_ALLOWED` |

The pool MUST evaluate the union required by the submitted request types, not
their net token flow. `from`, rather than `spender` or `to`, is the position
owner checked by the pool. The backstop checks the owner receiving active tier
shares. Repayment, reserve withdrawal, collateral withdrawal, claims, queueing
and matured withdrawal remain owner-authorized and do not require a positive
permission bit. Backstop donation, interest-auction fill, bad-debt fill, and
every permissionless accounting or recovery operation remain permissionless
under their inherited authentication and token-transfer checks.

A controller failure, malformed response, or unavailable state is not a
permission revocation. It MUST fail every operation that requires either a
positive permission or proof that a bit is absent. Ordinary repayment,
withdrawal, claims, and Q4W MUST NOT call the controller, so controller failure
cannot trap an owner in a risk-increasing position.

Permission absence immediately authorizes only the corresponding bounded
exit path; there is no additional offboarding flag or delay:

- When `RESERVE_BORROW_ALLOWED` is absent, any caller MAY invoke
  `new_forced_exit_auction(user)`. The target MUST have positive liabilities
  and collateral and no active user auction. A collateral-free borrower uses
  the inherited `bad_debt(user)` handoff instead. The pool selects all
  liability assets and a proportional amount of every collateral asset; the
  caller controls no
  asset, amount, price, percentage, or recipient. The result uses the inherited
  user-auction key, curve, lookup, stale deletion, proportional fill, position
  transfer, and completed-fill bad-debt handoff. The target MUST NOT use the
  ordinary healthy-position self-delete request to cancel a borrower-exit
  auction; stale deletion remains permissionless. The base bid contains every
  liability, while the base lot contains no more collateral than required by
  the inherited liquidation incentive unless all collateral is required.
- When `RESERVE_SUPPLY_ALLOWED` is absent and the target has no liabilities or
  active user auction, any caller MAY invoke
  `force_withdrawal(user, asset)`. It checkpoints the configured reserve
  and user, burns all of the target's ordinary-supply and collateral bTokens
  for that asset, and transfers the current underlying value only to the
  target. Zero-value shares, unreconciled custody, insufficient liquidity, or
  a failed token transfer MUST fail atomically. One asset is processed per
  invocation.
- When `BACKSTOP_DEPOSIT_ALLOWED` is absent, any caller MAY invoke
  `force_queue_withdrawal(tier, user, pool)`. It checkpoints the tier
  and user and queues all active shares under the inherited Q4W delay, entry
  bound, emission exclusion, take-rate eligibility, and loss exposure. After
  maturity, `force_withdrawal(tier, user, pool)` withdraws all currently
  matured queued shares only to the target. A failed tier-token transfer leaves
  the queue and accounting unchanged.

Permission revocation MUST NOT bypass the reserve oracle, utilization,
reconciliation, auction, bad-debt, Q4W, token authorization, or exact-balance
rules. It grants neither the controller nor pool admin custody and cannot
redirect user assets. Regranting a permission does not delete or invalidate an
auction or Q4W entry already created under a valid prior controller response.
The controller is pool-local policy, not protocol governance or an alternate
protocol-upgrade path.

## 5. Loss waterfall — **Replaced**

This replaces the single-token realization in `V2-AUCTION-003` and
`V2-AUCTION-004`; all other auction and supplier-default arithmetic is
inherited.

### 5.1 Tier-auction lifecycle

`V2-AUCTION-001` applies. Each pool retains at most one active auction of each
backstop type under the inherited public key and privately binds its selected
tier and settlement metadata. Private metadata has a 46-day temporary
lifetime; reads do not renew it. Pool records renew at 45 days to 46 days.

Each tier uses Section 4 valuation and `V2-AUCTION-004`'s inclusive 200-USDC
interest minimum. Interest fills donate to the selected tier; bad-debt fills
draw from it. Stale deletion releases the selection without changing
liabilities, pending credit, balances, or tier assets. If a selected
plain-USDC tier becomes deauthorized, the same deletion entry point may release
it immediately so the next permissionless creation can continue with a
transferable tier.

### 5.2 Bad-debt waterfall

One auction sells the first configured tier with positive transferable assets
and value under Section 4. A deauthorized plain-USDC tier is skipped. The
configured `FirstLoss`, `SecondLoss`, and optional `ThirdLoss` order is
otherwise immutable. Supplier loss begins only after every configured tier has
no usable transferable value.

The auction targets 120% of reserve-oracle-valued debt. Only the pool may authorize a
lot, which is the smaller of available tier tokens and the target amount. A
partial LP amount rounds up and requires linear per-share valuation so it
cannot underfill. Loss passed onward MUST NOT be understated.

Selection transfers no assets and changes no accounting or protocol weight.
Withdrawals remain blocked while the auction or liabilities exist. Expiry
leaves the persistent liabilities intact, and later prices do not resize the
selected lot.

Fills otherwise apply `V2-AUCTION-003`. The filler assumes dTokens and receives
the time-scaled selected tier token; no swap occurs. Only tokens actually
transferred impair active and queued shares. Untransferred base returns to
ordinary capacity. Residual dTokens after a discounted completion remain
liabilities. Private selected value falls proportionally with the remaining
base lot while original debt, target, and unfilled target remain creation
metadata.

When no bad-debt auction is active, permissionless
`new_auction(1, backstop, [], [], 100)` performs one bounded step. It validates
every backstop liability against the immutable reserve-index mapping and fails
on unknown or non-positive entries. The next batch follows immutable reserve
order and contains at most `min(max_positions - 1, 4)` reserves. The pool MUST
use canonical `pool_data` to select the first qualifying tier, and each call
restarts the strict tier search. It returns the v2-compatible `AuctionData`
projection when a tier qualifies and fails when none qualifies.
Callers MAY replace the empty `bid` with the exact canonical debt-asset set,
the empty `lot` with the expected selected-tier-token set, or both; a mismatch
fails the transaction without changing the selection.

The inherited permissionless `bad_debt(backstop)` supplier-default path
requires no active bad-debt auction and succeeds only when canonical
`pool_data` proves all tiers have no usable value. It revalidates liabilities,
accrues affected reserves, applies `V2-AUCTION-003`, clears the liability map,
and recomputes withdrawal eligibility atomically.

### 5.3 Take-rate allocation — **Replaced**

For reserve credit \(D\), eligible value \(R_i\), and the pool's immutable
tier weight \(w_i\):

\[
D_i=\left\lfloor
\frac{(D+C)w_iR_i}{\sum_jw_jR_j}
\right\rfloor,
\qquad C'=D+C-\sum_iD_i.
\]

A zero-value or deauthorized tier is omitted. Each pool and reserve stores one
pending amount per configured tier and its Section 1.1 carry. Section 3.3
determines eligible value, including capital selected for bad debt until drawn.

Persistent per-reserve tier amounts and carry are a direct-ledger client
boundary; reads do not renew TTL and report only live RPC entries. The pool
applies the immutable weights returned by canonical `pool_data`; the backstop
exposes no mutable weighting entry point.

With no active interest auction, any caller may invoke
`new_auction(2, backstop, [], reserve_assets, 100)` with one to
`min(max_positions - 1, 4)` unique configured reserves. The transaction follows
this atomic pipeline:

`bid = []` accepts whichever tier the canonical cursor and value checks
select. A caller MAY instead provide the exact expected selected-tier token.
A mismatch fails atomically and does not redirect the auction.

| Phase | Required behavior |
| --- | --- |
| Checkpoint | Accrue and persist the supplied reserves. Omitted reserves remain pending or uncheckpointed. |
| Allocate | Use one canonical `pool_data` snapshot and the formula above to move new credit into persistent tier amounts. |
| Select | Value pending reserve baskets with the immutable reserve oracle and, from the cyclic configured-tier cursor, choose the first transferable tier meeting the inclusive 200-USDC minimum. Previously allocated amounts for a deauthorized tier remain pending until reauthorization. |
| Create | Store the selected persistent amounts in a next-ledger auction, privately bind its tier, and advance the cursor. No second interest auction may start before fill or stale deletion. |
| Fill | Require a filler other than the pool or backstop and sufficient tier-token allowance. Derive a seven-decimal selected-tier bid worth 120% of the reserve lot, rounded up, transfer the realized reserve lot, and atomically transfer and account for the realized bid under the selected tier's rules below. |
| Recover | Stale deletion releases the selection; unfilled amounts remain in their original pending and credit accumulators. |

Public lookup and deletion remain `get_auction(2, backstop)` and
`del_auction(2, backstop)`, and fill validates the private tier. If the selected
plain-USDC tier becomes deauthorized, `del_auction` may cancel the auction
immediately rather than waiting for ordinary staleness. Selected amounts remain
pending so expiry cannot lose or reweight them; ordinary share operations
remain available. A partial fill releases its base-lot discount,
and only reserve assets actually transferred reduce pending amounts and
accrued credit. BLNT:XLM and BLNT:USDC bids are donated in full. Donation mints
no shares or user claim but appreciates active and queued shares and assumes
the tier's protocol roles. Configured weights govern credit allocation, not
necessarily the timing of realized donations.

For each plain-USDC or plain-XLM bid \(B\), the backstop transfers the full
amount from the filler but credits only 99% to the pool tier. It carries
fractional haircut dust per pool-tier in raw seven-decimal units:

\[
H=\left\lfloor\frac{B+C_h}{100}\right\rfloor,
\qquad C_h'=(B+C_h)\bmod 100,
\qquad B_{\mathrm{tier}}=B-H.
\]

The haircut \(H\) joins the global pending balance for its asset. Pending
USDC or XLM mints no shares, has no activation, loss, take-rate, or emission
role, and is not created by an unsolicited token transfer.

Any caller MAY invoke `buy_and_burn(asset)` for USDC or XLM. One call processes
\(X=\min(P,\lfloor R/200\rfloor)\), where \(P\) is that asset's pending balance
and \(R\) is its reserve in the matching canonical BLNT Comet. The backstop
reads that Comet's fee-inclusive paired-asset-per-BLNT spot price \(p\), sets
the maximum final price to \(\lceil1.01p\rceil\), and requires at least
\(\lfloor X10^7/p_{max}\rfloor\) BLNT from an exact-input swap. It verifies the
exact USDC decrease, exact BLNT receipt, reported final price, and exact BLNT
burn before reducing pending balance. A zero-work call returns zero. Any swap,
authorization, balance, or burn failure rolls back without changing pending
balance or the prior interest-auction settlement. No oracle or TWAP supplements
the canonical Comet spot; the reserve-fraction limit bounds one call's
exposure to current-spot manipulation.

## 6. BLNT emissions — **Extended**

### 6.1 V3 emitter launch, conversion, and replacement — **Replaced and extended**

`V2-EMISSIONS-004` is replaced for the initial v3 launch. V3 deploys a new
emitter already configured for the v3 backstop and canonical BLNT:USDC Comet
LP. It does not replace, reconfigure, or depend on the legacy BLND emitter,
which may continue serving v1 and v2 independently.

The v3 emitter preserves the legacy `initialize`, `distribute`, `drop`,
`get_last_distro`, `get_backstop`, and backstop-swap entry points and their
existing argument and return encodings. The legacy `initialize` parameter
named `blnd_token` MUST contain BLNT in a v3 deployment. Initialization MUST
reject a token equal to legacy BLND, either token using decimals other than
seven, or a BLNT Stellar Asset Contract whose administrator is not the v3
emitter. After initialization, `distribute` mints BLNT to the current backstop
at one token per elapsed second.

The emitter constructor immutably binds legacy BLND, records a one-time
initialization authority, and fixes an exclusive swap deadline exactly 60 days
after instantiation. `initialize` MUST require that authority and permanently
discard it on success so deployment cannot be front-run and no ongoing emitter
administrator remains. Before the deadline,
`swap_blnd_for_blnt(from, to, amount)` MUST require `from` authorization and a
positive amount, transfer exactly that many legacy BLND to the emitter, burn
the complete receipt, and mint the same raw seven-decimal amount of BLNT to
`to`. It MUST fail atomically on any balance mismatch, repeated entry,
overflow, identical token binding, or call at or after the deadline. The
deadline cannot be extended. `get_swap_deadline`, `get_legacy_blnd_token`, and
`get_total_swapped` expose the immutable deadline, legacy token, and cumulative
successful burn-and-mint amount. No BLNT-to-BLND path exists.

Because the v3 emitter recognizes the v3 backstop from its first checkpoint,
the backstop's first `distribute` activates ongoing accounting directly when
no earlier migration epoch or queue attestation exists. No emitter queue,
strict-balance contest, backfill interval, or legacy-emitter switch is required
for v3 launch. `drop` retains the existing one-call emitter lifecycle and may
mint an immutable deployment-selected initial BLNT allocation whose aggregate
does not exceed 50 million BLNT. Recipient selection, including an empty list,
remains deployment policy.

Future protocol versions retain the emitter replacement mechanism. Queueing
is permissionless and requires the candidate backstop to hold strictly more of
the current designated token than the incumbent; for the initial v3 binding,
that token is canonical BLNT:USDC LP. Equality is insufficient. The existing
31-day queue, cancellation, revalidation, final incumbent distribution,
recipient switch, and caller-selected next designated-token behavior remain
unchanged. This is the only protocol-upgrade route retained by the emitter;
v3 introduces no administrator or privileged replacement entry point.

### 6.2 Backstop-depositor emissions — **Extended**

`V2-BACKSTOP-005`, `V2-BACKSTOP-006`, `V2-EMISSIONS-001`, and
`V2-EMISSIONS-002` apply with the tier-aware, carry-conserving pipeline below.

After activation, each checkpoint verifies the emitter's returned one-BLNT-
per-second mint against its preceding checkpoint. A prior direct emitter call
is allocated once by the candidate's next call, while unrelated BLNT transfers
create no entitlement. The first positive call also verifies the exact BLNT
balance increase.

The inherited reward zone changes as follows:

- Entry requires Section 4's activation threshold. A pool with no eligible
  underlying BLNT may occupy an open slot but receives no BLNT allocation.
- Standalone removal requires failure of Section 4's activation threshold,
  regardless of eligible underlying BLNT.
- Full-zone replacement compares eligible underlying BLNT and remains strict.
- Before distribution begins, entry and standalone removal require no
  checkpoint; afterward they retain the inherited one-hour checkpoint.

A pool without either canonical BLNT LP may activate and enter an open
reward-zone slot but cannot earn backstop BLNT. For active canonical LP amount \(A_t\),
current Comet BLNT reserve \(R_t\), and LP supply \(S_t\), post-activation
weight is:

\[
B_t(A_t)=\left\lfloor\frac{A_tR_t}{S_t}\right\rfloor,
\qquad
B_p=B_{p,\mathrm{BLNT:USDC}}+B_{p,\mathrm{BLNT:XLM}}.
\]

Plain USDC, plain XLM, paired USDC/XLM, queued shares, and raw transfers
contribute no weight. A canonical LP earns regardless of its configured tier
position. Nonpositive supply, negative inputs, or aggregate accounted LP
greater than supply fails atomically. Composition is read in the same
invocation; checkpoint manipulation exposure is accepted and MUST be
disclosed.

All stages are bounded and use Section 1.1 at the stated scope:

| Stage | Required behavior and carry scope |
| --- | --- |
| Protocol split | Split emitted BLNT `7:3` between backstop and pool tranches; retain global split carry. |
| Pool allocation | Allocate both tranches across reward-zone pools by \(B_p\); retain separate global backstop and pool carries. |
| Tier allocation | Split each pool's backstop tranche between its configured canonical BLNT:USDC and BLNT:XLM tiers by their \(B_{p,t}\) values; retain pool-local tier carry and fix the result as pending BLNT. Later composition cannot redirect it. |
| Pool gulp | At most once per 24 hours, checkpoint both tier streams, replace each with a seven-day stream over pending plus its exact unstreamed predecessor, and grant the pool tranche through the inherited allowance. |
| Tier index | Advance each \(10^{14}\)-scaled cumulative index over active, nonqueued tier shares, retaining pool-tier schedule and index carries. No active shares means no depositor credit. |
| User index | Accrue each user from tier shares and index change, retaining user-tier carry. |

For the pool gulp, \(D=604800\), pending BLNT \(P\), remaining old seconds
\(r\), old scaled rate \(\epsilon\), and schedule carry \(C_s\) produce:

\[
Q=P10^7+r\epsilon+C_s,\qquad
\epsilon'=\left\lfloor\frac{Q}{D}\right\rfloor,\qquad C_s'=Q-D\epsilon'.
\]

Expiration is the gulp timestamp plus \(D\). A checkpoint at expiration includes
\(C_s'\) before clearing it, except that elapsed emissions with no active shares
remain uncredited as in v2.

For user shares \(S\), index change \(\Delta I\), and user carry \(C_u\):

\[
N=S\Delta I+C_u,\qquad
\Delta E=\left\lfloor\frac{N}{10^{14}}\right\rfloor,qquad
C_u'=N-\Delta E10^{14}.
\]

The inherited mutation checkpoints and new-share index apply independently per
tier. The same position cannot earn through two tiers or both eligible and
ineligible accounting.

Selected bad-debt capital remains eligible until transferred; only the actual
loss changes future weight. Selection, discount, stale release, composition,
membership, and status never rewrite prior allocation.

The inherited client estimate applies per tier but omits internal carries;
only `claim` is authoritative. Plain-USDC and plain-XLM tiers are ineligible.

The inherited owner-authorized claim selects one eligible tier and a nonempty
unique pool list. It aggregates that tier's accrual, performs one single-sided
BLNT Comet deposit, and credits the resulting LP proportionally to the same
owner and pool positions using v2 rounding. Exact BLNT and LP balance deltas
govern settlement; zero output or a mismatch fails atomically. A successful
claim increments the backstop-claimed BLNT counter. Compounded shares are
active but receive no retroactive credit. Canonical BLNT:USDC and BLNT:XLM
compound only into themselves and are claimed separately, regardless of tier
position; other tiers and the 30% pool tranche are ineligible.

### 6.3 Pool supply and borrow emissions — **Extended**

`V2-EMISSIONS-003` applies with these extensions:

- Configuration input is bounded to 60 entries before last-write-wins
  replacement. Empty configuration remains valid, but a gulp with no enabled
  positive weight fails before consuming candidate allocation; accrued BLNT
  remains available.
- A registered pool authorizes each gulp. Its inherited allowance increase and
  reserve-stream scheduling are atomic; rejection leaves the tranche pending.
- Floor remainder carries independently at pool, reserve-token, and user
  scopes. A later gulp checkpoints the seven-day stream, combines exact
  unvested BLNT, and restarts it. The reserve-token index scale is
  \(10^{d_r}\times10^7\); unrepresentable units and user subunits remain local
  carry.
- Bad-debt handoff and supplier default join the inherited position mutations
  that checkpoint affected streams and users. Removed identifiers retain
  existing streams.
- The owner authorizes a claim over a nonempty bounded list of unique valid
  identifiers and selects the recipient. Clearing and the inherited allowance
  payment are atomic and do not change the backstop-claimed counter.

## 7. Numeric and resource safety — **Extended**

`V2-SAFETY-001` and `V2-SAFETY-002` apply. The candidate backstop extends code
and instance TTL to 90 days at construction and whenever either falls below 89
days. V3-specific bounds are fixed in the sections above.
