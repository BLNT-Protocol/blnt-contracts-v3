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
unique asset from canonical BLND:XLM LP, BLND:USDC LP, USDC, and XLM and one
integer take-rate weight from 1 through 10. Weights MUST strictly decrease in
loss-waterfall order. The factory stores this immutable configuration with the
pool registration, and the backstop verifies and caches it before accepting the
pool.

The candidate also immutably binds the canonical BLND:USDC and BLND:XLM 80:20
Comets and canonical BLND, USDC, and XLM assets. No other backstop asset is
accepted and no backstop tier has an oracle configuration. Each canonical
Comet MUST be initialized unfrozen and then bound to a classic-account
controller whose signer weights are all zero and whose authorization
thresholds are positive. Deployment verification MUST reject any other
controller state. This makes the Comet's controller-only freeze and controller-
replacement operations permanently unreachable.

Every tier counts equally per verified USDC for activation, participates in
take-rate allocation using its configured weight, and absorbs loss in
configured order without a protocol-level haircut or concentration limit.
Only exact canonical BLND:USDC and BLND:XLM Comet tiers are BLND-emission
eligible. A pool with neither receives no BLND emissions. Plain-USDC and
plain-XLM interest proceeds have the buy-and-burn haircut in Section 5.3.

### 3.2 Position accounting

Each configured pool-tier independently applies `V2-BACKSTOP-001` and
`V2-BACKSTOP-002`; every deposit additionally verifies the pool through the
immutable factory before custody changes. Public share operations and
pool-authorized `donate` select a configured `BackstopTier`. The consolidated
`pool_data` returns an ordered tier vector containing asset identity, token,
configured weight, emission eligibility, tokens, shares, and USDC-equivalent
value, plus aggregate active value and value-weighted Q4W. Active BLND, queued
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

| Capital state | Activation/status | Loss | Take rate | Ongoing BLND |
| --- | --- | --- | --- | --- |
| Active accounted shares | Included | Available | Included | BLND-bearing tiers |
| Queued shares | Excluded | Available | Included | Excluded |
| Pool-selected bad-debt lot | Included until drawn | Selected for that auction | Included | Inherits active-share state until drawn |
| Raw direct transfer | Excluded | Excluded | Excluded | Excluded |

Prepared, partially filled, stale, and continued bad-debt auctions MUST retain
the corresponding v2 withdrawal-blocking liability until settlement or
supplier default clears it atomically.

### 3.4 Non-clawbackable custody — **Safety requirement**

The backstop exposes no issuer-clawback entry point. A deployment MUST prove
that every relevant issuer-controlled contract balance was created
non-clawbackable before accepting the deployment:

- the shared backstop's plain-USDC balance;
- each BLND and paired-asset SAC balance held by the canonical Comets; and
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

Each \(V_{p,i}\) is eligible pool-attributed value. Every verified USDC has
equal weight, so any combination—including one asset alone—may qualify.

\[
T_{\mathrm{activation}} = 12{,}500\ \mathrm{USDC}
\]

Equality qualifies. Falling below the threshold deactivates a pool, and
reactivation uses the same threshold.

The backstop immutably binds distinct BLND, USDC, and XLM tokens and the exact
BLND:USDC and BLND:XLM Comets. All five token interfaces MUST use seven
decimals. Construction rejects a Comet unless it contains exactly its expected
pair at normalized weights 80% BLND and 20% paired asset.

Let the BLND:USDC Comet hold BLND reserve \(B_u\), USDC reserve \(U\), and LP
supply \(S_u\). Its current reserve composition implies total USDC value
\(T_u=5U\). For BLND:USDC LP amount \(A_u\):

\[
V_u(A_u)=\left\lfloor A_u\frac{5U}{S_u}\right\rfloor
\]

The same Comet implies a BLND price of \(4U/B_u\). Let the BLND:XLM Comet hold
BLND reserve \(B_x\), XLM reserve \(X\), and LP supply \(S_x\). Its implied
total USDC value and the value of BLND:XLM LP amount \(A_x\) are:

\[
T_x=\left\lfloor\frac{5B_xU}{B_u}\right\rfloor,
\qquad
V_x(A_x)=\left\lfloor A_x\frac{T_x}{S_x}\right\rfloor
\]

For either Comet, underlying BLND is
\(B(A)=\lfloor AR_b/S\rfloor\). Every quote rechecks positive LP supply and
reserves and the immutable weights. Canonical USDC is valued one-for-one. The
same two Comets imply the USDC-equivalent value of plain-XLM amount \(A\):

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
than an external fair-market price. Swaps, one-sided liquidity changes,
donations, and issuer deauthorization effects can change them; BLND:USDC
remains the USDC and BLND-price anchor. The protocol does not automatically
pause activation, emissions, take-rate allocation, or auctions based on issuer
authorization state.

The consumers are activation, status, reward-zone membership, take-rate
allocation, auction sizing, and supplier-loss eligibility. Emission weight
instead recognizes only canonical BLND LPs and uses their same-invocation
underlying-BLND composition under Section 6.2.

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

### 4.3 User-liquidation handoff — **Safety extension**

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

### 4.4 Reserve clawback — **Added**

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
- Invoke the reserve SAC's `clawback(pool, amount)`. That invocation MUST
  require the current SAC administrator's authorization and MUST fail unless
  the pool's SAC balance entry is clawbackable.
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
liabilities, pending credit, balances, or tier assets.

### 5.2 Bad-debt waterfall

One auction sells the first configured tier with positive eligible assets and
value under Section 4. The configured `FirstLoss`, `SecondLoss`, and optional
`ThirdLoss` order is immutable. Supplier loss begins only after every
configured tier has no usable value.

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

A zero-value tier is omitted. Each pool and reserve stores one pending amount
per configured tier and its Section 1.1 carry. Section 3.3 determines eligible
value, including capital selected for bad debt until drawn.

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
| Select | Value pending reserve baskets with the immutable reserve oracle and, from the cyclic configured-tier cursor, choose the first tier meeting the inclusive 200-USDC minimum. |
| Create | Store the selected persistent amounts in a next-ledger auction, privately bind its tier, and advance the cursor. No second interest auction may start before fill or stale deletion. |
| Fill | Require a filler other than the pool or backstop and sufficient tier-token allowance. Derive a seven-decimal selected-tier bid worth 120% of the reserve lot, rounded up, transfer the realized reserve lot, and atomically transfer and account for the realized bid under the selected tier's rules below. |
| Recover | Stale deletion releases the selection; unfilled amounts remain in their original pending and credit accumulators. |

Public lookup and deletion remain `get_auction(2, backstop)` and
`del_auction(2, backstop)`, and fill validates the private tier. Selected
amounts remain pending so expiry cannot lose or reweight them; ordinary share
operations remain available. A partial fill releases its base-lot discount,
and only reserve assets actually transferred reduce pending amounts and
accrued credit. BLND:XLM and BLND:USDC bids are donated in full. Donation mints
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
and \(R\) is its reserve in the matching canonical BLND Comet. The backstop
reads that Comet's fee-inclusive paired-asset-per-BLND spot price \(p\), sets
the maximum final price to \(\lceil1.01p\rceil\), and requires at least
\(\lfloor X10^7/p_{max}\rfloor\) BLND from an exact-input swap. It verifies the
exact USDC decrease, exact BLND receipt, reported final price, and exact BLND
burn before reducing pending balance. A zero-work call returns zero. Any swap,
authorization, balance, or burn failure rolls back without changing pending
balance or the prior interest-auction settlement. No oracle or TWAP supplements
the canonical Comet spot; the reserve-fraction limit bounds one call's
exposure to current-spot manipulation.

## 6. BLND emissions — **Extended**

### 6.1 Migration lifecycle and backfill — **Extended**

`V2-EMISSIONS-004` applies to the v3 `distribute` and `drop` surface, clock,
10-million-BLND cap, skipped swap interval, 70/30 allocation, streams, and
initial-drop ceiling. V3 adds the following migration constraints:

- Every live emitter queue observed by `distribute` MUST designate this
  backstop and BLND:XLM LP. A compatible queue may be attested only during its
  final seven days and through seven days after unlock.
- No queue, a changed compatible queue, or expiration of that grace period
  clears stale attestation; an incompatible queue fails closed. Queue changes
  do not otherwise affect the inherited backfill clock.
- After replacement, `distribute` activates ongoing emissions only when the
  emitter recognizes this backstop and the observation occurs within the
  attested grace period. The deployed emitter's deleted queue is why prior
  attestation is required.
- Until activation succeeds after emitter replacement, canonical BLND LP tier
  mutations and reward-zone edits fail closed; other tier operations remain
  available.
- Backfill retains v2 pool weight, but only the canonical BLND:USDC tier,
  wherever configured, receives the 70% backstop tranche; every other tier is
  ineligible.
- `drop` records and verifies the exact scheduled backfill received by the
  backstop. Positive backstop-tier claims remain disabled until the complete
  schedule is funded. BLND:USDC claims and the 30% pool tranche then use their
  ordinary paths.

Recipient selection, including an empty list, remains deployment policy. V3
adds no migration view or replacement-mutating entry point.

### 6.2 Backstop-depositor emissions — **Extended**

`V2-BACKSTOP-005`, `V2-BACKSTOP-006`, `V2-EMISSIONS-001`, and
`V2-EMISSIONS-002` apply with the tier-aware, carry-conserving pipeline below.

After activation, each checkpoint verifies the emitter's returned one-BLND-
per-second mint against its preceding checkpoint. A prior direct emitter call
is allocated once by the candidate's next call, while unrelated BLND transfers
create no entitlement. The first positive call also verifies the exact BLND
balance increase.

The inherited reward zone changes as follows:

- Entry requires Section 4's activation threshold. A pool with no eligible
  underlying BLND may occupy an open slot but receives no BLND allocation.
- Standalone removal requires failure of Section 4's activation threshold,
  regardless of eligible underlying BLND.
- Full-zone replacement compares eligible underlying BLND and remains strict.
- Before distribution begins, entry and standalone removal require no
  checkpoint; afterward they retain the inherited one-hour checkpoint.

A pool without either canonical BLND LP may activate and enter an open
reward-zone slot but cannot earn backstop BLND. For active canonical LP amount \(A_t\),
current Comet BLND reserve \(R_t\), and LP supply \(S_t\), post-activation
weight is:

\[
B_t(A_t)=\left\lfloor\frac{A_tR_t}{S_t}\right\rfloor,
\qquad
B_p=B_{p,\mathrm{BLND:USDC}}+B_{p,\mathrm{BLND:XLM}}.
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
| Protocol split | Split emitted BLND `7:3` between backstop and pool tranches; retain global split carry. |
| Pool allocation | Allocate both tranches across reward-zone pools by \(B_p\); retain separate global backstop and pool carries. |
| Tier allocation | Split each pool's backstop tranche between its configured canonical BLND:USDC and BLND:XLM tiers by their \(B_{p,t}\) values; retain pool-local tier carry and fix the result as pending BLND. Later composition cannot redirect it. |
| Pool gulp | At most once per 24 hours, checkpoint both tier streams, replace each with a seven-day stream over pending plus its exact unstreamed predecessor, and grant the pool tranche through the inherited allowance. |
| Tier index | Advance each \(10^{14}\)-scaled cumulative index over active, nonqueued tier shares, retaining pool-tier schedule and index carries. No active shares means no depositor credit. |
| User index | Accrue each user from tier shares and index change, retaining user-tier carry. |

For the pool gulp, \(D=604800\), pending BLND \(P\), remaining old seconds
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
BLND Comet deposit, and credits the resulting LP proportionally to the same
owner and pool positions using v2 rounding. Exact BLND and LP balance deltas
govern settlement; zero output or a mismatch fails atomically. A successful
claim increments the backstop-claimed BLND counter. Compounded shares are
active but receive no retroactive credit. Canonical BLND:USDC and BLND:XLM
compound only into themselves and are claimed separately, regardless of tier
position; other tiers and the 30% pool tranche are ineligible.

### 6.3 Pool supply and borrow emissions — **Extended**

`V2-EMISSIONS-003` applies with these extensions:

- Configuration input is bounded to 60 entries before last-write-wins
  replacement. Empty configuration remains valid, but a gulp with no enabled
  positive weight fails before consuming candidate allocation; accrued BLND
  remains available.
- A registered pool authorizes each gulp. Its inherited allowance increase and
  reserve-stream scheduling are atomic; rejection leaves the tranche pending.
- Floor remainder carries independently at pool, reserve-token, and user
  scopes. A later gulp checkpoints the seven-day stream, combines exact
  unvested BLND, and restarts it. The reserve-token index scale is
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
