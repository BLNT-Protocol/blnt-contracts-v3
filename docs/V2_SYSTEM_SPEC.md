# Blend V2 Frozen Behavioral Specification

Status: Frozen compatibility baseline

Source commit: `ba22b487b2c5057a4ecc28b05b5193c28e4bd117`

## 1. Purpose and authority

This document records the behavior inherited from the frozen Blend v2
contracts. It is not a proposal to change deployed v2 and does not grant
authority to upgrade those contracts.

The upstream source at the pinned SDK-22 commit is the frozen source baseline.
If this document differs from the frozen source or tests, the executable
behavior wins and this document MUST be corrected.

Normative terms `MUST`, `MUST NOT`, `SHOULD`, and `MAY` describe the behavior a
v3 candidate inherits unless the v3 specification explicitly overrides it.

## 2. Frozen runtime and contracts

### V2-RUNTIME-001: Toolchain

The historical workspace MUST remain pinned to:

- Rust 1.81.
- Soroban SDK 22.0.7.
- `wasm32-unknown-unknown`.
- The dependency graph and artifacts recorded in `BASELINE.md`.

V2 MUST NOT be recompiled against the v3 SDK merely to share a crate graph.

### V2-ARCH-001: Contract roles

The baseline consists of:

- One v2 backstop that accepts a single BLND:USDC Comet LP token.
- One pool factory bound to that backstop and one immutable pool WASM hash.
- Isolated lending pools deployed by the factory.
- One immutable SEP-40 oracle binding per pool.

Pools share the backstop contract but retain independent reserve, user,
auction, configuration, and backstop-allocation state.

### V2-IMMUTABILITY-001: Contract code

The backstop, factory, and pool code expose no general
administrator-controlled WASM replacement path. Pool-local administration is
not protocol governance.

## 3. V2 backstop

### V2-BACKSTOP-001: Single accepted token

The v2 backstop accepts one immutable BLND:USDC Comet LP token. Its BLND and
USDC assets, BLND token, and pool factory are immutable constructor bindings.

An initial deposit for a pool MUST verify that the pool was deployed by the
configured factory. Accounted deposits are attributed to one pool and one
user. Direct token transfers do not mint shares or establish a user claim.

### V2-BACKSTOP-002: Pool shares

Each pool has one backstop-token asset balance and one share supply:

- The first deposit mints shares one-for-one with LP-token base units.
- Later deposits mint
  `floor(deposit * existing_shares / existing_tokens)`.
- A zero-share mint MUST fail.
- Redemptions use
  `floor(burned_shares * existing_tokens / existing_shares)`.
- Burning all remaining shares returns all remaining pool-attributed tokens.
- If the pool-attributed token balance is zero while shares remain, a deposit
  converts to zero shares and fails. A withdrawal that converts to zero tokens
  also fails; v2 does not burn a worthless queued share for zero assets.

A token gain or loss changes the common exchange rate for every active and
queued share in that pool. Pool-attributed value MUST NOT be used to satisfy
another pool's liability.

### V2-BACKSTOP-003: Queue for withdrawal

V2 uses a bounded queue-for-withdrawal:

- Queueing moves user shares out of the active balance and creates an expiry
  exactly 17 days later.
- A user may have at most 20 entries for one pool.
- Withdrawal consumes expired entries oldest-first.
- Dequeueing restores shares newest-first and may occur before or after expiry.
- Queued shares remain in contract custody and in the pool's total share
  supply until withdrawal.
- Queued shares receive no ongoing BLND-emission weight.
- Queued shares remain exposed to pool-token gains and losses through the
  common exchange rate.
- Withdrawal MUST fail while the pool has unresolved bad debt.

### V2-BACKSTOP-004: Activation threshold

The backstop derives the pool's underlying BLND and USDC from current Comet
reserves and LP supply, rounds each underlying pool balance down to a whole
token, and evaluates:

\[
\lfloor B_{\mathrm{BLND}}\rfloor^4
\times
\lfloor B_{\mathrm{USDC}}\rfloor
\ge 10^{25}
\]

Multiplication intentionally saturates because every saturated result is above
the threshold. Equality qualifies.

This threshold controls v2 status eligibility and reward-zone admission. It is
not a USDC market-value threshold.

### V2-BACKSTOP-005: Reward zone

The v2 reward zone:

- Is permissionless and contains at most 30 unique pools.
- Admits only pools meeting `V2-BACKSTOP-004`.
- Requires a distribution checkpoint within the preceding hour before a
  membership edit once the zone is nonempty.
- When full, replaces a named member only if the entrant has a strictly larger
  pool-attributed LP-token balance.
- Permits removal only after the member falls below the threshold.

Stored pool status is not an additional reward-zone gate.

### V2-BACKSTOP-006: Backstop emission weight

Reward-zone pool weight is its nonqueued pool-attributed BLND:USDC LP-token
amount. A pool may gulp its accrued allocation at most once per 24 hours. The
gulp applies `V2-EMISSIONS-002`, grants the 30% pool tranche through a BLND
allowance, and streams the 70% backstop tranche over seven days.

Within a pool, active user shares receive the streamed backstop allocation
through a cumulative index; queued shares receive zero weight. Deposits,
queueing, and dequeueing checkpoint the affected pool and user before changing
active shares, so new or dequeued shares do not receive earlier accrual. A
refreshed stream checkpoints its predecessor, truncates the predecessor's
unstreamed amount to seven decimals, adds the new allocation, and replaces it
with a fresh seven-day rate. For seven-day duration \(D\), pending BLND \(P\),
remaining seconds \(r\), and old scaled rate \(\epsilon\), v2 computes
\(U=\lfloor r\epsilon/10^7\rfloor\) and
\(\epsilon'=\lfloor(P+U)10^7/D\rfloor\).

For elapsed seconds \(t\) and active shares \(S\), a stream checkpoint adds
\(\lfloor t\epsilon10^7/S\rfloor\) to the \(10^{14}\)-scaled pool index,
bounded by expiration. A user checkpoint credits
\(\lfloor S_u\Delta I/10^{14}\rfloor\) BLND. Elapsed emissions while no active
shares exist produce no depositor credit. V2 discards every remainder in these
calculations.

The owner-authorized `claim(from, pools, min_lp_out)` accepts a nonempty list
of unique pool addresses, checkpoints each pool, and aggregates the owner's
accrued backstop BLND. It deposits the aggregate BLND single-sided into the
BLND:USDC Comet once and adds the resulting LP tokens proportionally to the
owner's selected pool positions using floor rounding. V2 does not pay claimed
backstop BLND directly to the owner's wallet and exposes no separate
claimable-emissions view. Clients MAY estimate from pool, user, and emission
records, but only `claim` performs the authoritative checkpoint.

## 4. Pool lifecycle and administration

### V2-POOL-ADMIN-001: Transferable pool admin

Each pool has one pool-local admin. Transfer uses:

1. `propose_admin` authorized by the current admin.
2. `accept_admin` authorized by the proposed admin.

A proposal alone grants no authority.

The admin MAY:

- Update the backstop take rate.
- Update maximum positions.
- Update minimum collateral.
- Queue or cancel reserve configuration.
- Select the bounded admin statuses.
- Configure relative supply and debt emission weights.

The admin cannot replace the backstop, factory, pool WASM, or any
protocol-wide rule.

### V2-POOL-CONFIG-001: Reserve configuration

During Setup, a valid queued reserve configuration may be applied immediately.
After Setup:

- An admin-authorized reserve change unlocks after seven days.
- The admin may cancel it before application.
- Any caller may apply it after unlock.
- A new reserve receives the next immutable index.
- Updating a reserve preserves its index and decimals.
- A nonzero liability factor cannot be changed to zero.
- Relevant interest-curve changes reset the reserve's interest modifier.

A pool supports at most 30 reserves. Reserve configuration validates factors,
utilization targets, rate ordering, supply cap, decimals, and enabled status as
encoded by the frozen implementation.

### V2-POOL-STATUS-001: Status codes

V2 defines:

| Code | Status | Selected by |
| ---: | --- | --- |
| 0 | Admin Active | Pool admin |
| 1 | Active | Permissionless refresh |
| 2 | Admin On-Ice | Pool admin |
| 3 | On-Ice | Pool admin or permissionless refresh |
| 4 | Admin Frozen | Pool admin |
| 5 | Frozen | Permissionless refresh |
| 6 | Setup | Construction |

On-Ice blocks new borrowing and auction cancellation. Frozen additionally
blocks supply and collateral supply. Repayment, withdrawals, liquidation and
auction fills, and actions not prohibited by the frozen action matrix remain
available subject to their ordinary checks.

### V2-POOL-STATUS-002: Q4W transitions

V2 calculates the queued percentage from queued shares divided by total
backstop shares, rounding up at seven-decimal precision. Its status boundaries
are:

- Ordinary On-Ice at 30% Q4W.
- Admin Active becomes On-Ice at 50% Q4W.
- Ordinary Frozen at 60% Q4W.
- Admin On-Ice becomes Frozen at 75% Q4W.

Failure of `V2-BACKSTOP-004` also produces ordinary On-Ice. Admin Active
requires the threshold and less than 50% Q4W. Admin On-Ice requires less than
75% Q4W. Admin Frozen may always be selected. Status 4 blocks permissionless
refresh; Setup cannot be refreshed.

## 5. Lending accounting

### V2-LENDING-001: Request model

The v2 pool supports:

| Code | Request |
| ---: | --- |
| 0 | Supply |
| 1 | Withdraw |
| 2 | Supply collateral |
| 3 | Withdraw collateral |
| 4 | Borrow |
| 5 | Repay |
| 6 | Fill user-liquidation auction |
| 7 | Fill bad-debt auction |
| 8 | Fill interest auction |
| 9 | Delete user-liquidation auction |

The request owner is `from`, incoming tokens are provided by `spender`, and
outgoing tokens are sent to `to`. Both submission entry points require
`spender` authorization. When `from` differs from `spender`, they additionally
require `from` authorization. Merely receiving outgoing tokens does not require
`to` authorization.

The ordinary `submit` entry point performs direct spender transfers.
`submit_with_allowance` instead has the pool perform `transfer_from` against
the spender's token allowance and nets incoming and outgoing amounts for each
asset; it does not remove either entry-point authorization requirement. Both
entry points accept an empty request list as an authenticated no-op and return
the owner's current positions.

The separate zero-fee `flash_loan` entry point requires `from` authorization.
It creates the requested borrow liability before processing the accompanying
requests, transfers the borrowed asset to the receiver contract, and invokes
`exec_op(from, asset, amount, 0)`. The accompanying requests settle through
the allowance path with `from` as owner, spender, and recipient. They may repay
the flash borrow or leave it as an ordinary healthy collateralized liability;
the final position, utilization, status, and atomicity checks still apply.
The event topics are `["flash_loan", asset, from, contract]`, and its vector
data is `[tokens_out, d_tokens_minted]`.

There is no unauthenticated repay-on-behalf path. A Soroban contract address
may own a position and authorize through its contract logic.

### V2-LENDING-002: bToken and dToken accounting

V2 represents supply and collateral with bToken shares and debt with dToken
shares:

- Supply and collateral mint bTokens by rounding down.
- Withdrawal burns bTokens by rounding up, capped by the position balance.
- Borrow mints dTokens by rounding up.
- Partial repayment burns dTokens by rounding down.
- Repayment computes the rounded-down dToken burn first. A request clears the
  complete debt when that burn is at least the position balance, but v2 treats
  it as an over-repayment only when the burn is strictly greater. In that
  over-repayment branch, direct submission collects the requested amount from
  `spender` and transfers the excess over live debt to `to`, while allowance
  submission nets the excess and pulls only live debt. If the rounded burn
  exactly equals the balance, both modes collect the complete requested amount
  even when it is slightly above live debt because of dToken rounding.

Reserve bToken and dToken supplies conserve their corresponding user
positions.

### V2-LENDING-003: Interest

V2 accrues its kinked utilization-based interest curve into:

- `d_rate`, which increases borrower debt;
- `b_rate`, which credits supplier yield; and
- reserve `backstop_credit`, which receives the configured take-rate portion.

The backstop take rate is a pool parameter in seven-decimal fixed point and
must remain below 100%. V2 has no separate protocol or treasury fee.

### V2-LENDING-004: Health and limits

V2:

- Values positions with the pool's immutable SEP-40 oracle.
- Rejects nonpositive or more-than-one-day-stale prices.
- Rounds effective collateral down and effective liabilities up.
- Enforces collateral and liability factors, minimum collateral, maximum
  positions, supply caps, and maximum utilization.
- Supports at most 60 effective collateral and liability positions per user.

Borrow and collateral withdrawal MUST leave the position at or above the
seven-decimal health-factor floor `1.0000100` and at or above the configured
minimum effective collateral value.

### V2-LENDING-005: Atomicity

A failed request MUST roll back position, reserve, auction, emission, event,
and token-transfer effects. Disabled reserves reject supply, collateral
supply, and borrow but permit withdrawal and repayment.

## 6. Auctions and loss

### V2-AUCTION-001: Shared price curve

The public v2 auction interface identifies an auction by `(auction_type,
user)`: type 0 is user liquidation, type 1 is bad debt, and type 2 is backstop
interest. Creation, lookup, and deletion use the generic `new_auction`,
`get_auction`, and `del_auction` entry points; fills use the corresponding
`submit` request discriminant.

V2 auctions begin on the ledger after creation and use a 400-ledger curve:

- During the first 200 ledgers, bid remains 100% while lot increases by 0.5%
  per ledger from zero to 100%.
- During ledgers 201 through 400, lot remains 100% while bid decreases by 0.5%
  per ledger toward zero.
- Percentage selection rounds the base bid up and base lot down before applying
  the time modifier.
- Unselected base amounts remain in a partial auction.
- An auction becomes stale after 500 ledgers and may be deleted through the
  applicable permissionless recovery path.

The auction record stores base bid and lot amounts. A fill applies the current
time modifier only to the selected transfer amounts; a partial fill reduces
the remaining base amounts. Completion removes the auction.

### V2-AUCTION-002: User liquidation

User-liquidation creation is permissionless. It:

- Requires an unhealthy liquidatee other than the pool or backstop.
- Accepts unique liability-bid and collateral-lot reserve lists.
- Requires a percentage from 1 through 100.
- Is bounded by the pool's maximum-position setting.
- Uses current reserve rates and oracle prices.
- Uses the v2 incentive and repair-band calculation.
- Treats an all-position liquidation above 95% as a checked full liquidation.

Filling transfers bToken collateral and dToken liability shares from the
liquidatee to the filler without changing aggregate reserve share supplies.
The filler cannot be the liquidatee and must satisfy the pool's ordinary health
and position constraints.

An account with an active liquidation auction cannot complete an ordinary
submission. It may cancel only when status permits and the resulting position
is healthy. Auction fills remain available while the pool is On-Ice or Frozen.

When a completed auction leaves liabilities and no collateral, v2 transfers
the residual dToken positions to the backstop for bad-debt resolution.

### V2-AUCTION-003: Bad debt

V2 creates at most one bad-debt auction per pool. The auction:

- Offers the pool's single BLND:USDC backstop token as lot.
- Uses a 120%-of-debt target under the frozen valuation and rounding rules.
- Transfers filled dToken liabilities from the backstop to the filler.
- Transfers the time-scaled LP-token lot to the filler.
- Reduces pool-attributed backstop assets without burning shares.

After the next-ledger start, an authenticated filler other than the pool or
backstop may fill from 1% through 100%. The resulting position must satisfy the
ordinary maximum-position, minimum-collateral, and health requirements.
Bad-debt fills remain available while the pool is On-Ice or Frozen.

If the v2 backstop cannot cover residual debt under its frozen minimum and
auction policy, supplier loss follows the v2 default calculation:

```text
default_asset_amount = ceil(d_tokens * d_rate / 10^12)
b_rate_loss = ceil(default_asset_amount * 10^12 / b_supply)
new_b_rate = max(previous_b_rate - b_rate_loss, 0)
```

The default removes the backstop's dToken position and corresponding reserve
`d_supply`, leaves `b_supply` unchanged, and intentionally saturates `b_rate`
at zero when the loss exceeds supplier value.

### V2-AUCTION-004: Backstop interest

Accrued reserve `backstop_credit` is realized through an interest auction:

- A pool has at most one active interest auction, identified by type 2 and the
  configured backstop address.
- Creation is permissionless and requires an oracle-valued reserve-asset lot
  of at least 200 USDC.
- The filler bids the single BLND:USDC backstop token.
- The filler receives the time-scaled reserve-asset lot.
- The bid is donated to the pool's backstop allocation without minting shares.
- The donation appreciates active and queued backstop shares through their
  common exchange rate.
- The auction uses `V2-AUCTION-001`.

V2 does not create a direct per-user interest claim.
Interest-auction creation and reserve-credit accounting are pool-local; the
backstop provides custody and receives the realized donation.

## 7. BLND emissions

### V2-EMISSIONS-001: Reward-zone allocation

After a valid distribution checkpoint, v2 allocates new BLND among reward-zone
pools in proportion to nonqueued BLND:USDC LP-token amounts. Integer division
rounds down under the frozen implementation, and v2 does not carry every
allocation remainder forward.

Standalone distribution requires at least five seconds since the preceding
checkpoint and a nonempty reward zone.

### V2-EMISSIONS-002: Immutable 70/30 split

Each pool's accrued allocation is split:

- 70% to its active backstop depositors.
- 30% to its configured supply and borrow positions.

The split is fixed in v2 and is not a pool-admin parameter.

### V2-EMISSIONS-003: Pool tranche

The pool admin supplies valid reserve-token identifiers and positive relative
weights for the 30% tranche. The frozen setter accepts an empty vector and
duplicate identifiers; for a duplicate, the last supplied weight wins.
Identifier `2r` is the reserve's dToken and `2r + 1` is its bToken.

A pool emission gulp:

- Is allowed at most once per 24 hours.
- Rejects an allocation below one BLND.
- Omits disabled configured reserves.
- Normalizes the remaining positive weights.
- Streams each allocation over seven days.
- Checkpoints existing streams before balance or configuration changes.

Supply emission weight includes ordinary and collateral bTokens. Debt emission
weight uses dTokens. A refreshed stream includes its unstreamed predecessor.
Claims require the position owner's authorization and may direct the BLND to a
chosen recipient through the pool's backstop allowance. Stream refresh and
index rounding follow the truncating pattern in `V2-BACKSTOP-006` at the
reserve-token scale.

### V2-EMISSIONS-004: Candidate backfill and initial drop

The v2 backstop constructor binds an immutable initial-drop recipient list. Its
aggregate allocation plus the maximum 10-million-BLND backfill MUST fit within
the emitter's 50-million-BLND drop ceiling.

Before the emitter recognizes the candidate backstop, permissionless
`distribute` implements queue-independent backfill:

- The first call records the current ledger timestamp and returns zero.
- Later calls accrue one BLND per eligible second, subject to the 10-million-
  BLND aggregate cap and the ordinary reward-zone, 70/30, gulp, stream, and
  claim rules.
- Backfill does not inspect, start, pause, reset, or extend the emitter's swap
  queue.
- Once the emitter reports a checkpoint for the candidate, the next
  `distribute` ends backfill, resets the candidate timestamp to that emitter
  checkpoint, and returns zero. The interval between the candidate's last
  pre-swap checkpoint and the emitter checkpoint receives no allocation.

Public `distribute` returns the newly allocated BLND amount. Public `drop`
submits the immutable recipient list plus the recorded candidate-backfill
amount to the emitter and returns no value. The emitter enforces its drop
lifecycle and ceiling; v2 does not add a separate migration-status entry
point. Clients MAY reconstruct lifecycle state from the contract's ledger
entries without extending their TTL.

## 8. Authorization

### V2-AUTH-001: User authority

Custody-changing operations require the authorization encoded by the frozen
entry point. Pool-local admin authority does not authorize movement of a
user's position or backstop shares.

## 9. Numeric and resource behavior

### V2-SAFETY-001: Arithmetic

V2 uses integer fixed-point arithmetic with explicit seven- and twelve-decimal
scales. Its exact floor, ceiling, checked, and intentional saturation behavior
is part of the inherited baseline.

V3 MUST NOT reinterpret a v2 rounding direction merely because a different SDK
or intermediate integer type is available.

### V2-SAFETY-002: Bounds and TTL

The frozen maximum reserve, position, reward-zone, and Q4W sizes are part of
the baseline. V2 has no separate request-count check. Persistent, temporary,
code, and instance TTL behavior is defined by the SDK-22 implementation and
tests.
