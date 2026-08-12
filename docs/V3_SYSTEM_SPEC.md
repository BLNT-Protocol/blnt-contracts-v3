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

The backstop immutably binds a factory and uses its `is_pool` interface to
identify registered pools. A deposit MUST confirm factory registration before
custody changes. Every registered pool MUST expose the v2-compatible
`get_positions(address)` interface used to guard withdrawals.

Each pool-tier independently applies `V2-BACKSTOP-002`. Checked `assets`,
`shares`, and `queued_shares` are isolated per pool and tier and grant no
cross-pool claim. Raw transfers create no shares or protocol role. Active and
queued partitions share one exchange rate, absorb gains and losses
proportionally, convert independently with floor rounding, and leave partition
dust until final redemption.

Public deposit, queue, dequeue, and withdrawal operations select one of the
three immutable assets through the `BackstopTier` discriminator.
Pool-authorized donation selects a tier and adds its token to that pool-tier
without minting shares, appreciating its active and queued positions.
The public `pool_data` view returns each tier's tokens, shares, and total USDC
value together with aggregate active USDC value and value-weighted Q4W. Active
BLND and queued-value details remain internal. V3 exposes no separate
pool-tier-state or pool-valuation view.

The public backstop surface is limited to user operations, pool callbacks, and
consolidated operational views. Internal pool and user emission indexes,
carries, and migration fields are not separate entry points.

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
| Pool-selected bad-debt lot | Included until drawn | Selected for that auction | Included | Inherits active-share state until drawn |
| Raw direct transfer | Excluded | Excluded | Excluded | Excluded |

User, pool, and global shares decrease only when an expired withdrawal
transfers custody out.

Every withdrawal reads the configured backstop's pool positions and fails
while any liability remains, matching v2. Prepared, partially filled, stale,
and continued bad-debt auctions MUST retain the corresponding backstop
liability until settlement or supplier default clears it atomically.

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
the canonical `pool_data` snapshot, and the pool applies the inherited status
matrix locally as in v2. Stored status does not separately gate the reward
zone; bad-debt and tier-interest entry points remain available under every
status.

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
  Auction pricing and submission behavior otherwise remain inherited.

All extensions share the inherited atomic rollback boundary.

### 4.3 User-liquidation handoff — **Safety extension**

`V2-AUCTION-001` and `V2-AUCTION-002` apply. When a completed liquidation
exhausts collateral while liabilities remain, the same transaction MUST:

- Move every residual dToken share from the liquidatee to the configured
  backstop without changing reserve dToken supply.
- Accumulate each amount with any liability the backstop already holds in that
  reserve.
- Update the configured backstop's canonical v2-style liability map.
- Clear the liquidatee's residual liabilities.
- Emit one handoff event per affected reserve.

The handoff does not modify or create a bad-debt auction. New liabilities
remain input to a later permissionless type-1 `new_auction` call.

An incomplete fill never hands off. It MUST fail if it exhausts collateral or
leaves collateral and liabilities while reducing health.

The inherited 30-reserve maximum bounds the handoff, whose liability-map
mutation uses one load and store and MUST fit Protocol-27 invocation limits.

## 5. Loss waterfall — **Replaced**

This replaces the single-token realization in `V2-AUCTION-003` and
`V2-AUCTION-004`; all other auction and supplier-default arithmetic is
inherited.

### 5.1 Tier-auction lifecycle

Each pool may have one active bad-debt auction and one active interest auction.
The public v2-style auction identity is `(auction_type, user)`. For both
backstop auction types, `user` MUST equal the pool's configured backstop. The
pool privately stores the selected tier and settlement metadata with a 46-day
temporary lifetime. Reads do not renew auction state.

Bad-debt selection uses every tier with positive accounted assets and value.
Interest-auction creation requires at least 200 USDC in the selected tier's
pending lot; equality qualifies. A successfully valued smaller interest lot
is ineligible, while unavailable, negative, or inconsistent valuation fails
closed.

A partial fill removes selected base amounts and applies the inherited time
modifier only to actual transfers. An interest fill atomically donates the
realized bid to its tier; a bad-debt fill atomically draws the realized loss.
Pool records use a 45-day renewal threshold and 46-day bump. Completion deletes
the record. At the inherited 500-ledger stale boundary, permissionless deletion
releases the selection without changing liabilities, pending credit, balances,
or tier assets.

### 5.2 Bad-debt waterfall

1. BLND:XLM LP.
2. BLND:USDC LP.
3. Plain USDC.
4. Pool suppliers.

Sections 3.1 and 3.3 govern eligibility. One auction sells one tier, selecting
the first with positive accounted assets and value. Zero-value tiers are
skipped; negative or inconsistent accounting or valuation stops the search.
Supplier loss begins only after all three tiers have no usable value.

The auction targets 120% of oracle-valued debt. Only the pool may authorize a
lot, which is the smaller of available tier tokens and the target amount. A
partial LP amount rounds up and requires linear per-share valuation so it
cannot underfill. Loss passed onward MUST NOT be understated.

Selection transfers no assets and changes no accounting, activation value,
shares, exchange rate, take-rate weight, or emission weight. The pool permits
no second bad-debt auction, and its withdrawal callback blocks withdrawals
while the auction or liabilities remain. Expiry leaves persistent liabilities
blocking withdrawal, and later price movement does not resize a selected lot.

Fills otherwise apply `V2-AUCTION-001` and `V2-AUCTION-003`. The time modifier
scales only dTokens and tier tokens transferred, while selected base amounts
reduce the auction. The filler assumes dTokens and receives the selected tier
token; no swap occurs. Only tokens actually transferred reduce accounted
assets, impairing active and queued shares without burning them. Untransferred
base returns to ordinary capacity. Residual dTokens after a complete
late-discount fill remain liabilities and block withdrawal. A partial fill
reduces selected value in proportion to the remaining base lot; original
debt, target, and unfilled target remain creation metadata.

When no bad-debt auction is active, permissionless
`new_auction(1, backstop, [], [], 100)` performs one bounded step. It validates
every backstop liability against the immutable reserve-index mapping and fails
on unknown or non-positive entries. The next batch follows immutable reserve
order and contains at most `min(max_positions - 1, 4)` reserves. The pool MUST
use canonical `pool_data` to select the first qualifying tier, and each call
restarts the strict tier search. It returns the v2-compatible `AuctionData`
projection when a tier qualifies and fails when none qualifies.

As in v2, the separate permissionless `bad_debt(backstop)` entry point handles
supplier default. It requires no active bad-debt auction, repeats the canonical
liability validation, and succeeds only when canonical `pool_data` proves that
all three tiers have no usable value. The transaction accrues each affected
reserve, applies `V2-AUCTION-003` to at most 30 reserves, clears the liability
map, and recomputes withdrawal eligibility atomically.

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
Section 3.3, using full accounted value even when selected for bad debt.

Each pool and reserve stores three pending tier amounts and an isolated carry.
New credit plus carry is apportioned by the formula; floors and new carry
conserve the exact input without combining scopes.

As in v2, the pool contract exposes no separate interest-reserve accounting
view. Clients MAY inspect the persistent per-reserve allocation through the
SDK's direct ledger reader; the stored key and value encoding are therefore a
client-compatibility boundary. A direct read does not extend TTL and reports
whether the persistent entry exists in the RPC server's live-ledger view.

As in v2, the pool owns take-rate and interest-auction policy. It applies the
immutable weights locally using the backstop's canonical `pool_data` values;
the backstop exposes no separate weighting or allocation entry point.

When no interest auction is active, any caller may invoke
`new_auction(2, backstop, [], reserve_assets, 100)` with one to
`min(max_positions - 1, 4)` unique configured reserves. Creation atomically:

1. Accrue and persist every reserve in the supplied batch.
2. Checkpoint each asset's newly available backstop credit into its three
   pending tier amounts using the immutable pool-local weighting formula and
   one canonical `pool_data` snapshot.
3. Value each tier's pending amounts with the pool's immutable reserve oracle.
4. Starting at the pool's stored cyclic tier cursor, select the first
   qualifying tier whose pending lot meets the 200-USDC minimum.
5. Store the selected pending amounts in a next-ledger auction and advance
   the cursor to the next tier.

Omitted reserves remain pending or uncheckpointed. Selection is cyclic among
qualifying available tiers. The next tier cannot be selected until the active
interest auction is filled or stale-deleted. Public lookup and deletion use
`get_auction(2, backstop)` and `del_auction(2, backstop)`; fill validates the
privately stored selected tier. Ordinary backstop share operations remain
available while an auction is active. The selected lot remains in persistent
pending accounting so expiry cannot lose or reweight it.

The selected seven-decimal tier token is the bid. The pool derives its amount
from canonical `pool_data`; its base value MUST equal 120% of the reserve lot,
rounded up in token units.

A partial interest fill releases any base-lot discount from the reservation.
Only reserve assets actually transferred from the pool are subtracted from
both the persistent pending tier amount and accrued backstop credit.

The authorized filler cannot be the pool or backstop and MUST grant the
backstop sufficient tier-token allowance. The pool transfers the realized
reserve lot and atomically invokes tier-aware `donate` for the realized bid.
The bid enters the selected pool-tier without minting shares or a user claim,
appreciating active and queued shares and immediately assuming that tier's
protocol roles. Unfilled amounts remain in their original accumulators after
stale recovery. The `4:3:2` ratio allocates credit lots; auction timing may
produce a different ratio of realized donated value.

## 6. BLND emissions — **Added and extended**

### 6.1 Migration lifecycle and backfill — **Added**

The backstop retains v2's permissionless `distribute` and `drop` migration
surface around the incumbent emitter's 31-day backstop-swap queue. The first
pre-replacement `distribute` starts a queue-independent backfill clock. A caller
queues and executes replacement through the emitter's existing
`queue_swap_backstop` and `swap_backstop` functions. Every live queue observed
by `distribute` must designate this backstop and BLND:XLM LP. A compatible
queue must be attested no earlier than its final seven days and no later than
the end of the post-unlock grace period.

As in v2, public `distribute` returns the allocated BLND amount and public
`drop` returns no value. The SDK reconstructs the exact lifecycle snapshot from
the contract-instance storage; the contract adds no migration view or
replacement mutating APIs.

Before replacement, the first `distribute` initializes the accounting epoch at
its invocation time and returns zero; later calls checkpoint backfill whether
or not an emitter queue exists. Queue creation, cancellation, or replacement
does not start, pause, reset, or extend that clock. After the clock starts, a
current distribution checkpoint is required before BLND-bearing weight
changes, preventing later weight from receiving retroactive credit.
Observing no queue, a different compatible queue, or the end of the grace
period clears a stale attestation; an incompatible queue fails closed. The
bounded attestation is required because the deployed emitter deletes its queue
and exposes no designated-token getter after replacement.

After the emitter swap, `distribute` detects the registered candidate and
activates only through seven days after the attested unlock. It ends backfill
at the most recent pre-replacement checkpoint, resets the distribution
timestamp to the emitter's candidate checkpoint, and returns zero. This
deliberately matches v2 and omits any uncheckpointed interval between the final
candidate call and the swap. No interval receives both backfill and ongoing
BLND. Until this transition succeeds, BLND-bearing weight mutations and
reward-zone edits fail closed; plain-USDC operations remain available.

The first pre-replacement `distribute` opens the ordinary allocation epoch at
one BLND per eligible second, capped at 10 million BLND. Backfill uses the same
70/30 split, reward zone, pending-pool accounting, and seven-day streams
specified in Sections 6.2 and 6.3, except that active, nonqueued BLND:USDC
LP-token amounts determine pool weight directly, matching v2. The 70% tranche
is pending only for BLND:USDC; BLND:XLM and plain USDC receive no backfill. A
pool gulp starts or refreshes the BLND:USDC stream, and active shares earn as
that stream advances. Queueing first realizes streamed accrual and then stops
future stream weight; dequeueing resumes at the current index.

The constructor binds an immutable initial-drop recipient list. Its aggregate
allocation plus the maximum 10-million-BLND backfill MUST NOT exceed the
emitter's 50-million-BLND ceiling. Choosing recipients, including choosing an
empty list, is deployment policy rather than protocol policy.

After activation, `drop` may be called once. It submits the configured list
and, when positive, the exact scheduled backfill for this backstop. The
configured BLND balance MUST increase by the total amount directed to the
backstop. Backstop-tier claims remain disabled until the full positive schedule
is funded. Backfill uses the ordinary claim paths: BLND:USDC claims compound
into that tier and the 30% pool tranche uses the inherited pool allowance and
claim accounting.

### 6.2 Backstop-depositor emissions — **Extended**

`V2-BACKSTOP-006` and `V2-EMISSIONS-002` apply. For distributable BLND plus
prior carry:

\[
E_{\mathrm{backstop}} =
\left\lfloor E_{\mathrm{total}}\frac{7}{10}\right\rfloor,\qquad
E_{\mathrm{pool}} =
\left\lfloor E_{\mathrm{total}}\frac{3}{10}\right\rfloor,\qquad
C_{\mathrm{next}} =
E_{\mathrm{total}} - E_{\mathrm{backstop}} - E_{\mathrm{pool}}
\]

The candidate retains \(C_{\mathrm{next}}\) for the next split. Both tranches
use the same reward-zone pool weight. The pool tranche remains pending under
Section 6.3, while the backstop tranche becomes tier-specific pending BLND and
does not enter a depositor index until a pool gulp.

After activation, the distributable amount is one BLND per elapsed emitter
checkpoint second, matching v2. Each call verifies the emitter's returned mint
for the interval since its immediately preceding checkpoint. A prior direct
emitter call is therefore allocated once at the next candidate checkpoint,
while unrelated BLND transfers create no emission entitlement. The first
positive candidate call also verifies an exact configured-BLND balance delta.

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

Only reward-zone pools participate. After activation, pool weight is active,
nonqueued underlying BLND across the two BLND-bearing tiers:

\[
B_p =
B_{p,\mathrm{BLND:USDC}} +
B_{p,\mathrm{BLND:XLM}}
\]

\[
E_{x,p} =
\left\lfloor E_x
\frac{B_p}{\sum_{q \in \mathrm{reward\ zone}} B_q}
\right\rfloor
\quad
(x\in\{\mathrm{backstop},\mathrm{pool}\})
\]

Plain USDC and the paired USDC/XLM portions contribute no weight. Bounded
implementation uses one accumulator per eligible tier and scoped carry.

The checkpoint fixes \(E_{\mathrm{backstop},p}\) between the two tiers:

\[
E_{p,t} =
\left\lfloor
(E_{\mathrm{backstop},p} + C_{p,\mathrm{tier}})
\frac{B_{p,t}}{B_p}
\right\rfloor
\]

The unassigned sum becomes pool-local tier carry. Later composition changes
cannot redirect pending BLND between tiers or across the migration boundary.

At most once per 24 hours, a permissionless pool gulp checkpoints each existing
tier stream, combines its exact unstreamed remainder with pending tier BLND, and
starts a fresh seven-day stream. With \(D=604800\), pending amount \(P_{p,t}\),
old seconds remaining \(r_{p,t}\), old scaled rate
\(\epsilon_{p,t}\), and schedule carry \(C_{p,t}^{\mathrm{schedule}}\):

\[
Q_{p,t} =
P_{p,t}10^7 + r_{p,t}\epsilon_{p,t}
+ C_{p,t}^{\mathrm{schedule}},\qquad
\epsilon'_{p,t} = \left\lfloor\frac{Q_{p,t}}{D}\right\rfloor
\]

The new expiration is the gulp timestamp plus \(D\), and the remainder
\(Q_{p,t}-D\epsilon'_{p,t}\) remains tier-local carry. At a stream checkpoint,
elapsed scaled emissions advance that tier's \(10^{14}\)-scaled cumulative
index over its active, nonqueued shares. An expiration checkpoint includes the
schedule remainder so a completed stream does not strand a whole token unit.

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
dequeue, or withdrawal changes active shares, the tier stream and user's index
MUST checkpoint. New and dequeued shares start at the current index; queueing
first realizes current streamed accrual. Elapsed emissions with no active
shares receive no depositor credit, matching v2.

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
depositors. Every rounding remainder carries at its original scope.

After distribution begins, deposits, queue/dequeue, withdrawals, actual loss,
or auction gain that changes a reward-zone pool's active BLND-bearing LP amount
requires a production checkpoint no more than five seconds old. Other pools
and plain USDC are exempt. A mutation cannot change prior allocation and may
affect only the unresolved five-second window.

Queued shares have zero weight; dequeue resumes at the current index. Selected
bad-debt capital remains eligible until a fill transfers it, at which point
only the transferred amount loses weight. Selection, discount, stale release,
balance or composition changes, membership, and pool status never rewrite
prior allocation. A position MUST NOT earn through two tiers or both eligible
and ineligible accounting.

As in v2, the contract exposes no claimable-emissions view. Clients MAY
estimate one eligible tier's aggregate claim by reading its pool balance, user
balance, pool emission, and user emission records. The estimate omits
contract-internal rounding carries and is non-authoritative; `claim` performs
the authoritative checkpoint and includes every retained carry. Plain USDC is
ineligible.

The owner authorizes a claim for one eligible tier over a nonempty list of
unique registered pool addresses and supplies a minimum LP output. The claim
checkpoints that tier in each pool, aggregates its accrued 70% tranches, and
deposits the BLND single-sided into that tier's Comet once. Resulting LP tokens
are credited proportionally to the same owner's selected pool-tier positions,
using v2 floor rounding. Exact aggregate BLND and LP balance deltas govern
settlement. A failed or zero-output conversion leaves every accrual unchanged.
A successful claim updates the backstop-claimed counter by the BLND consumed
and returns the aggregate LP amount received.

BLND:USDC accrual compounds into BLND:USDC and BLND:XLM accrual compounds into
BLND:XLM. Claims are separate so one impaired Comet cannot block the other.
Compounded shares are active backstop capital, receive no retroactive emission
credit. Plain USDC is ineligible. The operation cannot include the 30% pool
tranche, redirect entitlement, or move a claim between tiers.

### 6.3 Pool supply and borrow emissions — **Extended**

`V2-EMISSIONS-003` applies with these extensions:

- Configuration input is bounded to 60 entries before last-write-wins
  replacement. Empty configuration remains valid, but a gulp with no enabled
  positive weight fails before consuming candidate allocation; accrued BLND
  remains available.
- A registered pool authorizes a gulp of \(E_{\mathrm{pool},p}\), subject to the
  inherited 24-hour and one-BLND minimums. The backstop increases that pool's
  configured-BLND allowance by the gulped amount, and the pool schedules its
  reserve-token streams atomically. Rejection leaves the accrued tranche
  available.
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
  and pays the recipient with `transfer_from` against its backstop allowance.
  Pool state and transfer are atomic and do not change the backstop-claimed
  counter.

## 7. Numeric and resource safety — **Extended**

`V2-SAFETY-001` and `V2-SAFETY-002` apply. The candidate backstop extends code
and instance TTL to 90 days at construction and whenever either falls below 89
days. V3-specific bounds are fixed in the sections above.
