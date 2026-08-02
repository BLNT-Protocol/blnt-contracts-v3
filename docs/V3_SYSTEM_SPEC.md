# Blend V3 Contract Specification

Status: Draft 0.2

## 1. Purpose and inheritance

This document defines only the v3 additions, replacements, extensions,
approved exceptions, and safety fixes. Unstated behavior inherits
[V2_SYSTEM_SPEC.md](V2_SYSTEM_SPEC.md) under [SYSTEM_SPEC.md](SYSTEM_SPEC.md).
A difference MUST be required by the three-tier backstop, Protocol 27, an
approved conservation or safety fix, or an exception recorded here.

Scoped integer carry-forward is an approved conservation fix. The activation
hysteresis in Section 4 is the sole approved economic exception.
Implementation details MUST NOT introduce a protocol fee, another allocation,
a protocol-wide administrator or multisig, governance, an emergency override,
a privileged WASM replacement, or another upgrade path. Inherited pool-admin
and user authorization remain pool-local and custody-local; deployment actors
receive no continuing authority. A future protocol-wide change requires a
separately specified and deployed contract version.

Normative terms `MUST`, `MUST NOT`, `SHOULD`, and `MAY` describe requirements.

## 2. Runtime — **Replaced**

The frozen SDK-22 v2 baseline is governed by `V2-RUNTIME-001`. New v3 contracts
MUST target Stellar Protocol 27 with Rust 1.91.1, Soroban SDK 27.0.3, and the
`wasm32v1-none` target. V3 does not need to compile against SDK 22 or any
intermediate SDK version.

## 3. Multi-asset backstop — **Replaced and extended**

This section replaces the single accepted token and generalizes the
pool-share and Q4W accounting in `V2-BACKSTOP-001` through
`V2-BACKSTOP-003` only as stated below.

### 3.1 Asset configuration

The candidate immutably binds exactly these three seven-decimal assets:

| Tier | Asset | USDC valuation | Ongoing BLND weight |
| ---: | --- | --- | --- |
| 1 | BLND:XLM 80:20 LP | Comet-implied valuation | Yes |
| 2 | BLND:USDC 80:20 LP | Comet-implied valuation | Yes |
| 3 | Plain USDC | One-for-one | No |

All three count equally per verified USDC for activation, participate in the
fixed-weight take-rate policy, and absorb loss in table order without an
asset-specific haircut or concentration limit. No other tier is supported.
Eligibility follows Section 3.3.

### 3.2 Position accounting

The backstop immutably binds a factory whose `backstop` and `is_pool`
interfaces MUST identify the candidate and its registered pools. Before
custody changes, a deposit MUST confirm factory registration and the pool's
fail-closed
`backstop_withdrawal_allowed(backstop)` interface, but its current boolean
result does not block new loss-absorbing capital. Canonical valuation requires
factory registration without calling back into the pool. Every
factory-deployed pool MUST preserve this withdrawal interface.

Each pool-tier independently applies `V2-BACKSTOP-002`. Checked `assets`,
`shares`, and `queued_shares` are isolated per pool and tier; global per-tier
totals provide only a bounded conservation check and grant no cross-pool
claim. Raw transfers create no shares or protocol role. Active and queued
partitions share one exchange rate, absorb gains and losses proportionally,
convert independently with floor rounding, and leave partition dust until
final redemption.

Public deposit, queue, dequeue, and withdrawal operations select one of the
three immutable assets through the `BackstopTier` discriminator.

As a v3 liveness safety fix, expired shares in a fully drained tier MAY be
burned for zero assets. A new deposit remains prohibited while worthless
shares exist and may initialize a fresh one-to-one exchange rate only after all
outstanding shares are removed. No administrator or external caller can adjust
an exchange rate directly.

### 3.3 Withdrawals

Each tier independently inherits the 17-day delay, oldest-first withdrawal,
newest-first dequeue, and common-exchange-rate loss exposure in
`V2-BACKSTOP-003`. One user may have at most 20 aggregate entries for one pool
across all tier queues.

Capital state has these canonical policy effects:

| Capital state | Activation/status | Loss | Take rate | Ongoing BLND |
| --- | --- | --- | --- | --- |
| Active accounted shares | Included | Available | Included | BLND-bearing tiers |
| Queued shares | Excluded | Available | Included | Excluded |
| Bad-debt committed assets | Excluded | Reserved for that loss | Included | Inherits active-share state until transferred |
| Raw direct transfer | Excluded | Excluded | Excluded | Excluded |

User, pool, and global shares decrease only when an expired withdrawal
transfers custody out.

An interest-auction commitment blocks deposits and actual withdrawals only in
its selected tier. Each pool may hold at most one such commitment per tier and
three total. Queueing and dequeueing remain available. Permissionless deletion
releases the selected tier's lock at the inherited 500-ledger stale boundary.

Every withdrawal calls
`backstop_withdrawal_allowed(backstop)` on the attributed pool. The callback
MUST identify the immutable backstop and find no liability, committed loss, or
unresolved bad debt. Amounts remain reserve-keyed and are never summed across
asset units. A missing, failed, negative, nonzero, or inconsistent record fails
closed. Borrow, repay, liquidation handoff, bad-debt preparation, fill,
continuation, stale release, and supplier default update these records
atomically. Interest commitments use the selected-tier lock instead.

## 4. Pool activation — **Replaced**

This replaces `V2-BACKSTOP-004`. In seven-decimal USDC units:

\[
E_p =
V_{p,\mathrm{BLND:XLM}} +
V_{p,\mathrm{BLND:USDC}} +
V_{p,\mathrm{USDC}}
\]

Each \(V_{p,i}\) is eligible pool-attributed value. Every verified USDC has
equal weight, so any combination—including one asset alone—may qualify.

\[
T_{\mathrm{entry}} = 12{,}500\ \mathrm{USDC}
\qquad
T_{\mathrm{maintenance}} = 10{,}000\ \mathrm{USDC}
\]

Equality qualifies. Falling below maintenance deactivates a pool; reactivation
requires the entry threshold.

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
BLND reserve \(B_x\) and LP supply \(S_x\). Its implied total USDC value and
the value of BLND:XLM LP amount \(A_x\) are:

\[
T_x=\left\lfloor\frac{5B_xU}{B_u}\right\rfloor,
\qquad
V_x(A_x)=\left\lfloor A_x\frac{T_x}{S_x}\right\rfloor
\]

For either Comet, underlying BLND is
\(B(A)=\lfloor AR_b/S\rfloor\). Every quote rechecks positive LP supply and
reserves and the immutable weights. Zero LP needs no quote; invalid,
incompatible, negative, or overflowing inputs fail atomically. Plain USDC is
valued one-for-one. Active and queued pool-attributed LP amounts are quoted
separately.

These values deliberately reflect current Comet composition rather than an
external fair-market price. Swaps, one-sided liquidity changes, donations, and
issuer deauthorization effects can change them; BLND:USDC remains the USDC
anchor. The protocol does not automatically pause activation, emissions,
take-rate allocation, or auctions based on issuer authorization state.

This valuation governs activation, status, reward-zone membership, take-rate
allocation, auction sizing, and supplier-loss eligibility. Emission weight
uses the same-invocation underlying-BLND composition defined separately in
Section 6.2.

### 4.1 Pool-status valuation — **Extended**

`V2-POOL-ADMIN-001`, `V2-POOL-CONFIG-001`, `V2-POOL-STATUS-001`, and
`V2-POOL-STATUS-002` apply with \(Q_p\) measured by verified USDC value:

\[
Q_p =
\left\lceil
\frac{V_{p,\mathrm{queued}}}
{V_{p,\mathrm{active}} + V_{p,\mathrm{queued}}}
10^7
\right\rceil
\]

Active and queued values use the same canonical inputs; take-rate weights do
not apply. A zero denominator gives \(Q_p=0\). Comparisons use seven-decimal
precision and round upward.

The inherited inclusive 30%, 50%, 60%, and 75% transitions and bounded admin
overrides apply unchanged. Statuses 0 and 1 use Section 4's
maintenance threshold. Statuses 2 through 6 are inactive and MUST meet its
entry threshold before returning to an active status. Statuses 4 and 6
continue to reject permissionless refresh.

Queueing does not change stored status. Refresh and admin requests use
canonical valuation. Stored status does not separately gate the reward zone;
bad-debt and tier-interest entry points remain available under every status.

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
- Tier-interest and bad-debt processes use dedicated direct entry points.

All extensions share the inherited atomic rollback boundary.

### 4.3 User-liquidation handoff — **Safety extension**

`V2-AUCTION-001` and `V2-AUCTION-002` apply. When a completed liquidation
exhausts collateral while liabilities remain, the same transaction MUST:

- Move every residual dToken share from the liquidatee to the configured
  backstop without changing reserve dToken supply.
- Accumulate each amount with any liability the backstop already holds in that
  reserve.
- Update every affected canonical reserve-keyed liability record.
- Clear the liquidatee's residual liabilities.
- Emit one handoff event per affected reserve.

The handoff does not modify or create a bad-debt auction. New liabilities
remain input to a later permissionless continuation.

An incomplete fill never hands off. It MUST fail if it exhausts collateral or
leaves collateral and liabilities while reducing health.

The inherited 30-reserve maximum bounds the handoff, whose liability-map
mutation uses one load and store and MUST fit Protocol-27 invocation limits.

## 5. Loss waterfall — **Replaced**

This replaces the single-token realization in `V2-AUCTION-003` and
`V2-AUCTION-004`; all other auction and supplier-default arithmetic is
inherited.

### 5.1 Shared tier-auction lifecycle

Each pool may have one active bad-debt auction and one active interest auction
per tier. Creation atomically stores matching pool and backstop records for one
identifier, selected tier, and base amounts with a 46-day temporary lifetime.
Only the registered pool may mutate its commitment; reads do not renew it.

Bad-debt eligibility requires at least 100 USDC of available tier capital;
interest-auction creation requires at least 100 USDC in the selected tier's
pending lot. Equality qualifies. A successfully valued smaller amount is
ineligible, while unavailable or invalid valuation fails closed.

A partial fill removes selected base amounts, applies the inherited time
modifier only to actual transfers, and synchronizes both records using a
45-day renewal threshold and 46-day bump. Completion deletes both. At the
inherited 500-ledger stale boundary, permissionless deletion releases the
commitment without changing liabilities, pending credit, balances, or tier
assets. All effects commit or roll back together.

### 5.2 Bad-debt waterfall

1. BLND:XLM LP.
2. BLND:USDC LP.
3. Plain USDC.
4. Pool suppliers.

Sections 3.1 and 3.3 govern eligibility. One auction sells one tier, selecting
the first with at least 100 USDC of eligible uncommitted capital. Smaller
tiers are skipped without a debt-covering exception; valuation failure stops
the search. Supplier loss begins only after all three successfully value below
the minimum, so less than 300 USDC may be skipped. Skipped capital remains
attributed and may requalify before supplier settlement.

The auction targets 120% of oracle-valued debt. Only the pool may authorize a
lot, which is the smaller of available tier tokens and the target amount. A
partial LP amount rounds up and requires linear per-share valuation so it
cannot underfill. Loss passed onward MUST NOT be understated.

Commitment transfers no assets and changes no shares or exchange rate.
Committed assets are excluded from activation and later loss lots. Expiry
leaves persistent liabilities blocking withdrawal, and later price movement
does not resize a committed lot.

Fills otherwise apply `V2-AUCTION-001` and `V2-AUCTION-003`. The time modifier
scales only dTokens and tier tokens transferred, while selected base amounts
reduce the auction. The filler assumes dTokens and receives the selected tier
token; no swap occurs. Only tokens actually transferred reduce accounted
assets, impairing active and queued shares without burning them. Untransferred
base becomes uncommitted capacity. Residual dTokens after a complete
late-discount fill remain liabilities and block withdrawal. A partial fill
reduces committed value in proportion to the remaining base lot; original
debt, target, unfilled target, and valuation validity remain creation metadata.

When no bad-debt auction is active, permissionless
`continue_bad_debt_resolution` performs one bounded step. It validates every
reserve-keyed backstop liability and fails on unknown, missing, negative, or
inconsistent records. The next batch follows immutable reserve order and
contains at most `min(max_positions - 1, 4)` reserves. The pool and backstop
MUST use the same quote to select and commit the first qualifying tier, and
each continuation restarts the strict tier search.

Only a successful no-tier result may default all remaining liabilities to
suppliers. The transaction accrues each affected reserve, applies
`V2-AUCTION-003` to at most 30 reserves, clears the liability map, and
recomputes withdrawal eligibility atomically.

### 5.3 Take-rate allocation — **Replaced**

Tier weights are `4:3:2` in loss order: BLND:XLM, BLND:USDC, and plain USDC.
For pool-level reserve credit \(D\), verified eligible value \(R_i\), and
weight \(w_i\):

\[
D_i =
\left\lfloor
D
\frac{w_i R_i}{\sum_j w_j R_j}
\right\rfloor
\]

A zero-value tier is omitted and the formula renormalizes. Eligibility follows
Section 3.3, using full accounted value even when committed to bad debt.
Take-rate and bad-debt commitments coexist but remain independently atomic.

Each pool and reserve stores three pending tier amounts and an isolated carry.
New credit plus carry is apportioned by the formula; floors and new carry
conserve the exact input without combining scopes.

Any caller may create one tier-specific interest auction for a pool by
supplying a unique identifier and one to `min(max_positions - 1, 4)` unique
configured reserves. Creation atomically:

1. Accrue and persist every reserve in the supplied batch.
2. Checkpoint each asset's newly available backstop credit into its three
   pending tier amounts through one canonical backstop weighting quote.
3. Value each tier's pending amounts with the pool's immutable reserve oracle.
4. Starting at the pool's stored cyclic tier cursor, select the first
   qualifying tier without an active auction whose pending lot meets the shared
   minimum.
5. Reserve the selected pending amounts in a next-ledger auction and advance
   the cursor to the next tier.

Omitted reserves remain pending or uncheckpointed. Selection is cyclic among
qualifying unlocked tiers. Each pool may have one active auction per tier and
three total; active identifiers must be unique within the pool. Get, fill, and
stale-delete operations identify the tier. The reserved lot remains in
persistent pending accounting so expiry cannot lose or reweight it.

The selected seven-decimal tier token is the bid. Its verified base value MUST
equal 120% of the reserve lot, rounded up in token units.

A partial interest fill releases any base-lot discount from the reservation.
Only reserve assets actually transferred from the pool are subtracted from
both the persistent pending tier amount and accrued backstop credit.

The authorized filler cannot be the pool or backstop. Exact source-balance
deltas govern both transfers. The bid enters the selected pool-tier without
minting shares or a user claim, appreciating active and queued shares and
immediately assuming that tier's protocol roles. Unfilled amounts remain in
their original accumulators after stale recovery. The `4:3:2` ratio allocates
credit lots; auction timing may produce a different ratio of realized donated
value.

## 6. BLND emissions — **Added and extended**

### 6.1 Migration lifecycle and backfill — **Added**

The backstop exposes a permissionless `Pending`, `Open`, `Prepared`, and
`Active` lifecycle around the incumbent emitter's 31-day backstop-swap queue.
Construction starts prefunding and fixes a 138-day absolute deadline. The
original queue must start within 31 days of construction. Preparation is
limited to the queue's final seven days, requires the candidate to directly
hold strictly more BLND:USDC LP than the incumbent, and permanently anchors
the recovery horizon to the original unlock. At most two retry queues may be
verified within the original unlock plus 76 days.

`begin_migration` creates the queue and opens its accounting epoch;
`open_migration_epoch` records an already observable valid queue. Every queue
must designate this backstop and BLND:XLM LP. `finalize_migration` rechecks the
prepared queue and BLND:USDC majority, invokes the emitter's backstop swap,
and atomically activates ongoing accounting. If another caller invokes that
swap first, `sync_migration` may activate the prepared candidate only through
seven days after the verified unlock. While such a candidate remains
unsynchronized, BLND-bearing weight mutations and reward-zone edits fail
closed; plain-USDC operations remain available.

The original queue opens the ordinary emission indexes at one BLND per eligible
second, capped at 10 million BLND. Canonical finalization checkpoints through
its transition timestamp; synchronization checkpoints only through the
verified unlock. Backfill uses the same 70/30 split, reward zone, carries, and
cumulative indexes specified in Sections 6.2 and 6.3, except that only active,
nonqueued underlying BLND in BLND:USDC LP contributes to either pool-level or
depositor-level weight. BLND:XLM LP and plain USDC receive no backfill.
Queueing checkpoints accrued BLND and stops future weight; dequeueing resumes
at the current index. Queueing or withdrawal never forfeits already accrued
BLND.

Successful migration awards no discretionary BLND. If the positive schedule
is nonzero, `fund_backfill` may be called once after activation and must request
exactly that amount from the emitter for this backstop alone. An exact increase
in the configured BLND balance is required. Backstop and pool claims remain
disabled until the full positive schedule is funded. Backfill uses the ordinary
claim paths: BLND:USDC claims compound into that tier and the 30% pool tranche
uses its ordinary reservation and claim accounting.

### 6.2 Ongoing backstop-depositor emissions — **Extended and safety fixed**

`V2-EMISSIONS-002` applies, but a successful checkpoint assigns the 70% tranche
immediately through the tier indexes below instead of the
`V2-BACKSTOP-006` seven-day stream. For distributable BLND plus prior carry:

\[
E_{\mathrm{backstop}} =
\left\lfloor E_{\mathrm{total}}\frac{7}{10}\right\rfloor,\qquad
E_{\mathrm{pool}} =
\left\lfloor E_{\mathrm{total}}\frac{3}{10}\right\rfloor,\qquad
C_{\mathrm{next}} =
E_{\mathrm{total}} - E_{\mathrm{backstop}} - E_{\mathrm{pool}}
\]

The candidate retains \(C_{\mathrm{next}}\) for the next split. Both tranches
use the same reward-zone pool weight; the pool tranche remains segregated
under Section 6.3.

The maximum-30-pool permissionless reward zone in `V2-BACKSTOP-005` applies
with these changes:

- Entry requires Section 4's entry threshold and positive eligible underlying
  BLND.
- Ordinary removal requires failure of Section 4's maintenance threshold.
- A zero-underlying-BLND member may be removed regardless of activation value
  and without a checkpoint.
- Full-zone replacement compares eligible underlying BLND and remains strict.
- Before distribution begins, entry and ordinary removal require no
  checkpoint; afterward they retain the inherited one-hour checkpoint.

Pool activation remains independent of reward-zone admission. A plain-USDC-only
pool may activate but cannot enter or earn until active, nonqueued
BLND-bearing capital gives it positive weight. Stored pool status does not
otherwise gate membership, distribution, or accrued emissions.

Only reward-zone pools participate. Pool and user weights are active,
nonqueued underlying BLND across the two BLND-bearing tiers:

\[
B_p =
B_{p,\mathrm{BLND:USDC}} +
B_{p,\mathrm{BLND:XLM}},\qquad
B_{p,u} =
B_{p,u,\mathrm{BLND:USDC}} +
B_{p,u,\mathrm{BLND:XLM}}
\]

\[
E_{x,p} =
\left\lfloor E_x
\frac{B_p}{\sum_{q \in \mathrm{reward\ zone}} B_q}
\right\rfloor
\quad
(x\in\{\mathrm{backstop},\mathrm{pool}\}),
\qquad
E_{\mathrm{backstop},p,u} =
\left\lfloor E_{\mathrm{backstop},p}
\frac{B_{p,u}}{B_p}
\right\rfloor
\]

Plain USDC and the paired USDC/XLM portions contribute no weight. Bounded
implementation uses one accumulator per eligible tier and scoped carry.

The checkpoint splits \(E_{\mathrm{backstop},p}\) between the two tiers:

\[
E_{p,t} =
\left\lfloor
(E_{\mathrm{backstop},p} + C_{p,\mathrm{tier}})
\frac{B_{p,t}}{B_p}
\right\rfloor
\]

The unassigned sum becomes pool-local tier carry
\(C_{p,\mathrm{tier}}\). Later composition changes cannot rewrite the split.

For active tier shares \(S_{p,t}\), each eligible tier uses a
\(10^{14}\)-scaled cumulative index:

\[
N_{p,t}=E_{p,t}10^{14}+C_{p,t,\mathrm{index}},\qquad
\Delta I_{p,t} =
\left\lfloor \frac{N_{p,t}}{S_{p,t}} \right\rfloor,\qquad
C'_{p,t,\mathrm{index}} =
N_{p,t} - \Delta I_{p,t}S_{p,t}
\]

For user shares \(S_{p,t,u}\):

\[
N_{p,t,u} =
S_{p,t,u}(I_{p,t}-I^{\mathrm{last}}_{p,t,u})
+ C_{p,t,u},\qquad
\Delta E_{p,t,u} =
\left\lfloor \frac{N_{p,t,u}}{10^{14}} \right\rfloor,\qquad
C'_{p,t,u} =
N_{p,t,u} - \Delta E_{p,t,u}10^{14}
\]

Carries remain at their pool-tier or user scope. Before deposit, queue,
dequeue, or withdrawal changes active shares, the user's index MUST
checkpoint. New and dequeued shares start at the current index; queueing first
realizes current accrual. No path grants retroactive credit.

At each allocation checkpoint, underlying BLND uses same-invocation Comet
reserves:

\[
B_t(A) =
\left\lfloor
A\frac{R_{\mathrm{BLND},t}}{S_t}
\right\rfloor
\]

Here \(A\) is accounted active LP, \(R\) the Comet's BLND balance, and \(S\)
LP supply. Nonpositive supply, negative inputs, or \(A>S\) fail closed. Direct
transfers are excluded and plain USDC has zero weight. This calculation reads
Comet composition directly rather than substituting its USDC-value output;
checkpoint manipulation exposure is accepted and MUST be
disclosed.

Pool and user allocation uses bounded accumulators without iterating over all
pools or depositors. Every remainder carries at its original scope.

After distribution begins, deposits, queue/dequeue, withdrawals, actual loss,
or auction gain that changes a reward-zone pool's active BLND-bearing LP amount
requires a production checkpoint no more than five seconds old. Other pools
and plain USDC are exempt. A mutation cannot change prior allocation and may
affect only the unresolved five-second window.

Queued shares have zero weight; dequeue resumes at the current index. Committed
bad-debt capital remains eligible until a guarded fill transfers it, at which
point only the transferred amount loses weight. Commitment, discount, stale
release, balance or composition changes, membership, and pool status never
rewrite prior allocation. A position MUST NOT earn through two tiers or both
eligible and ineligible accounting.

The owner authorizes a claim for one eligible tier and supplies a minimum LP
output. The claim checkpoints that tier for one pool, reduces only its accrued
70% tranche, deposits the BLND single-sided into that tier's Comet, and credits
the resulting LP shares to the same owner, pool, and tier. Exact BLND and LP
balance deltas govern settlement. A failed or zero-output conversion leaves the
accrual unchanged. A successful claim updates backstop-claimed and total-claimed
counters by the BLND consumed and returns the LP amount received.

BLND:USDC accrual compounds into BLND:USDC and BLND:XLM accrual compounds into
BLND:XLM. Claims are separate so one impaired Comet cannot block the other.
Compounded shares are active backstop capital, receive no retroactive emission
credit. Plain USDC is ineligible. The operation cannot include the 30% pool
tranche, redirect entitlement, or move a claim between tiers.

### 6.3 Pool supply and borrow emissions — **Custody extension**

`V2-EMISSIONS-003` applies with these extensions:

- Configuration input is bounded to 60 entries before last-write-wins
  replacement. Empty configuration remains valid, but a gulp with no enabled
  positive weight fails before reserving candidate allocation; accrued BLND
  remains available.
- The candidate retains pool-tranche custody. A registered pool authorizes
  reservation of \(E_{\mathrm{pool},p}\), subject to the inherited 24-hour and
  one-BLND minimums. Rejection leaves it available; reservation grants no
  allowance and is not a claim.
- Floor remainder carries independently at pool, reserve-token, and user
  scopes. A later gulp checkpoints the seven-day stream, combines exact
  unvested BLND, and restarts it. The reserve-token index scale is
  \(10^{d_r}\times10^7\); unrepresentable units and user subunits remain local
  carry.
- Every supply, collateral, borrow, repay, liquidation transfer, bad-debt
  handoff, supplier default, and applicable configuration mutation checkpoints
  the affected stream and user first. Late entrants receive no prior accrual,
  and removed identifiers retain existing streams.
- The owner authorizes a claim over a nonempty bounded list of unique valid
  identifiers and selects the recipient. The pool clears only those accruals
  and authorizes exact consumption of its reservation. Pool state, reservation,
  claimed accounting, and transfer are atomic. This increases total ongoing
  claimed BLND, not the backstop-claimed counter.

## 7. Numeric and resource safety — **Extended**

`V2-SAFETY-001` and `V2-SAFETY-002` apply. The candidate backstop extends code
and instance TTL to 90 days at construction and whenever either falls below 89
days. V3-specific bounds are fixed in the sections above.
