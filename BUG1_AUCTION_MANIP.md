# BUG1 — Auction quotes are frozen at a manipulable Comet spot valuation

**Verdict: confirmed, MEDIUM** — downgraded from HIGH, see the revision note.
The mechanism is real and reproduced end-to-end against the real `comet.wasm` in
`test-suites`: a single transaction creates a permanently mispriced auction that
the protocol never revalidates. What changed is the impact assessment. The Dutch
curve prices the mispriced *auction* legs back to par under competitive fillers
(§4), so the severity now rests on the non-auction consumers of the same spot
number — activation, pool status, and reward-zone eligibility and weight (§3.3),
which have no clearing mechanism behind them.

Scope of this document: SCAN.md finding 1 only. Analysis performed on `main` at
`f5e03b1`, working tree clean.

**Revision note.** The first draft rated this HIGH on the strength of §3.2's
break-even table, which assumed a rational filler waits for the full lot at
elapsed 200. That assumption is wrong — see §4 and `auction_manip_crossover.rs`.
The auction-leg loss at competitive equilibrium is ~zero, not 80% of the lot.
§3.2 and §4 are corrected below; §3.1, §3.3, §3.4 and §5 stand as written.

---

## 1. The primitive

Every backstop valuation is a *live* read of Comet reserves. `read_comet`
(`backstop/src/backstop/pool.rs:709`) calls `get_total_supply` / `get_balance`
on the Comet contract at call time and stores nothing:

```rust
let total_supply = client.get_total_supply();
let blnt_reserve = client.get_balance(blnt);
let pair_reserve = client.get_balance(pair);
```

Two quantities are derived from it and exposed as unauthenticated views on the
backstop (`backstop/src/contract.rs:343` `pool_data`, `:347` `blnt_price`):

| quantity | formula | consumer |
|---|---|---|
| `blnt_price` (`pool.rs:446`) | `4 * anchor.pair_reserve * 1e7 / anchor.blnt_reserve` | protocol-fee bid |
| BLNT:USDC tier `value` (`pool.rs:576`) | `lp_amount * (5 * anchor.pair_reserve) / anchor.total_supply` | interest bid, bad-debt lot, activation, pool status |
| BLNT:XLM tier `value` (`pool.rs:465`) | `lp_amount * target.blnt_reserve * 5 * anchor.pair_reserve / (anchor.blnt_reserve * target.total_supply)` | same |
| plain XLM tier `value` (`pool.rs:530`) | `amount * anchor.pair_reserve * target.blnt_reserve / (anchor.blnt_reserve * target.pair_reserve)` | same |

Note the third and fourth rows: the **BLNT:USDC anchor is a single point of
control over tiers the attacker never touches.** BLNT:XLM and plain-XLM tiers
carry the full `pair_reserve / blnt_reserve` anchor ratio, so they are *more*
sensitive to an anchor swap than the BLNT:USDC tier itself, which carries only
`pair_reserve`. This is the concrete shape of the v3 blast-radius widening.

`AGENTS.md:178-182` mandates this design ("Backstop value uses current reserves
… Do not introduce a backstop price oracle"), so the reserve read itself is not
the defect. The defect is that a quote taken from it is **stored and reused on a
later ledger without revalidation.**

## 2. Why the quote sticks

Three v3 auction creators bake the spot number into persistent auction state:

- `pool/src/auctions/tier_interest.rs:434` — `bid = ceil(ceil(lot_value*6/5) * tokens / value)`.
  Bid ∝ 1/`value`.
- `pool/src/auctions/protocol_fee_auction.rs:73` — `bid = ceil(target_value * 1e7 / blnt_price)`.
  Bid ∝ 1/`blnt_price`.
- `pool/src/auctions/tier_bad_debt.rs:395` — `lot = ceil(target_value * tokens / value)`.
  Lot ∝ 1/`value`, and `:391` short-circuits to **the entire tier** when
  `value <= 1.2 * debt_value`.

All three set `block = sequence + 1` and are reached from `new_auction`
(`pool/src/contract.rs:677`), which has **no `require_auth`** — `create_auction`
(`pool/src/auctions/auction.rs:96`) only validates asset-set shape and
`percent == 100`.

I confirmed no revalidation on the fill side: `fill_interest_auction`,
`fill_protocol_fee_auction`, and `fill_bad_debt_auction` call `scale_*` on the
stored map only. Neither `pool_data` nor `blnt_price` is called at fill.

Cancellation is not available in time. `del_tier_auction`
(`auction.rs:435`) requires `AUCTION_STALE_LEDGERS = 500` (~42 min) unless the
selected tier is a deauthorized-USDC tier or the interest lot holds a
deauthorized reserve — neither triggers on price skew. Meanwhile
`has_tier_auction` / `storage::has_auction` blocks any competing honest auction
of the same type.

## 3. Reproduction

Six tests against the real `comet.wasm`, in `test-suites/tests/`:

| test | shows | status |
|---|---|---|
| `auction_manip.rs` | frozen quote survives the unwind (§3.1) | **fails** — asserts the fill charges honest value |
| `auction_manip_cost.rs` | inflation cost curve (§3.2) | passes |
| `auction_manip_tiers.rs` | anchor moves untouched tiers (§3.3) | passes |
| `auction_manip_deflate.rs` | deflation cost curve (§3.4) | passes |
| `auction_manip_crossover.rs` | Dutch curve clears at par (§4) | passes |
| `auction_manip_eviction.rs` | permissionless reward-zone eviction (§3.5) | passes |

`auction_manip.rs` is written as a regression test: it asserts the property a
fix must provide, so it is red while the bug is present. The other four are
measurements and pass either way. Run with
`cargo test -p test-suites --test auction_manip -- --nocapture`.

Fixture anchor pool: 100,001,000 BLNT / 2,500,025 USDC (80:20, 0.3% fee),
`blnt_price = 0.1000000`. That is a realistically-sized ~$12.5M Comet pool.

### 3.1 End-to-end: discounted protocol-fee bid survives the unwind

`auction_manip.rs` — swap, create, unwind, then fill 200 ledgers later:

```
honest blnt_price = 1000000
skewed blnt_price = 3049926 (3x)
restored blnt_price = 1004807
round-trip USDC cost = $12,018
skewed bid = 786.9 BLNT
honest bid ~= 2388.5 BLNT
filler took 200 STABLE for 786.9 BLNT (honest price would demand 2388.5)
```

The bid discount tracks the price multiple exactly (3.03x vs 3.05x). The auction
is created and the pool is restored **inside the same transaction**; the stored
bid never moves.

### 3.2 Cost curve — inflation direction (interest + protocol-fee bids)

`auction_manip_cost.rs`. Comet caps one swap at 1/3 of the in-reserve
(`ErrMaxInRatio`, `c_consts.rs:13`), so the attacker compounds swaps:

| swaps | spot multiple | USDC deployed | USDC cost | saving | break-even lot |
|---|---|---|---|---|---|
| 1 | 1.32x | $625,006 | $3,126 | 29.2% of lot | **$10,709** |
| 2 | 1.74x | $1,406,264 | $6,168 | 51.3% | $12,028 |
| 3 | 2.30x | $2,382,836 | $9,130 | 68.0% | $13,427 |
| 4 | 3.04x | $3,603,551 | $12,018 | 80.7% | $14,901 |
| 5 | 4.03x | $5,129,445 | $15,156 | 90.2% | $16,799 |

Cost is essentially just the two swap fees, because both legs execute with no
intervening trade. The deployed capital is round-trip within one transaction, so
it is flash-loanable.

> **Correction.** The `saving` and `break-even lot` columns are wrong and are
> retained only to show what the first draft claimed. Both were computed as if
> the filler takes the *full* lot at the *full* mispriced bid, i.e. at elapsed
> 200. A competitive filler does not wait that long: the bid modifier is pinned
> at 100% for the whole ramp, so the auction becomes profitable as soon as the
> partial lot exceeds the frozen bid, and the clearing rate there is ~1:1. See
> §4. The `spot multiple`, `USDC deployed` and `USDC cost` columns are measured
> and remain valid — the attack costs what this table says; it just does not
> earn what this table says.
>
> Corrected reading: **there is no break-even lot size for the auction legs.**
> The manipulation moves an auction earlier along its own curve and captures
> approximately nothing.

### 3.3 The anchor moves tiers it does not contain — *primary finding*

`auction_manip_tiers.rs`, four swaps on **BLNT:USDC only**:

```
                 before          after         multiple
BlntXlm tier    $12,500        $38,124          3.05x
BlntUsdc tier   $62,500       $152,587          2.44x
active_value    $75,000       $190,711          2.54x
```

The BLNT:XLM tier — whose own Comet pool was untouched — moved *more* than the
pool that was actually traded, exactly as the formula table predicts.

`active_value` is the same number that gates `meets_activation_threshold`
(`pool/src/pool/status.rs:94`) and reward-zone entry/exit
(`backstop/src/emissions/manager.rs:44,82`), and `add_reward`
(`backstop/src/contract.rs:377`) is likewise unauthenticated. Reward-zone
*weight* uses a separate spot read, `pool_spot_blnt_emission_weight`
(`backstop/src/emissions/policy.rs:86`), so the entrant-vs-incumbent weight
comparison in `add_to_reward_zone` is manipulable in the same transaction.

**This is the leg that carries the severity.** §4 shows the auction legs are
self-correcting under competitive fillers, because a Dutch auction has a
clearing mechanism that reprices a bad quote. These consumers do not. They are
instantaneous reads feeding a threshold comparison — `active_value >= 12,500`,
entrant weight vs. incumbent weight — with no curve, no filler, and no second
chance. A 2.54x swing held for the length of one transaction flips the boolean,
and the state change it authorizes (activation, reward-zone admission or
eviction) persists long after the reserves are restored. §3.5 drives this
end-to-end.

### 3.4 Cost curve — deflation direction (bad-debt lot)

`auction_manip_deflate.rs`, pushing BLNT in to crush tier value. Tier holds
50,000 LP honestly worth $62,500:

| swaps | tier value | BLNT round-trip cost | bad debt that sweeps the whole tier |
|---|---|---|---|
| 1 | 41% | ~$11,996 | $21,384 |
| 2 | 16% | ~$24,007 | $8,780 |
| 3 | 6% | ~$36,032 | $3,605 |
| 4 | 2% | ~$48,072 | $1,480 |
| 6 | 0.5% | ~$72,194 | $249 |

Read the first row: for $11,996 the attacker turns a $21,384 bad debt — which
honestly warrants ~20,529 LP ($25,661) — into a lot of **all 50,000 LP
($62,500)**, an overpayment of $36,839. This is the `available_value <=
target_value` short-circuit at `tier_bad_debt.rs:391`.

Deflation is materially more expensive than inflation, because it drains the
20%-weighted USDC side against the 80%-weighted BLNT side. It also needs bad debt
to already exist. It is the strictly less practical of the two directions, but it
is the one with unbounded loss per event.

### 3.5 Permissionless reward-zone eviction — *the leg that actually pays*

`auction_manip_eviction.rs` drives §3.3's value swing into an irreversible state
change. `remove_from_reward_zone` (`manager.rs:74`) evicts any member whose
`active_value` is below the activation threshold, and `remove_reward`
(`contract.rs:384`) has no `require_auth`:

```
honest active_value = $62,500 (threshold $12,500)
  try_remove_reward at honest reserves -> Err   (pool is qualified)
after 2 BLNT-in swaps: active_value = $10,536 (16% of honest)
  remove_reward -> Ok                           (pool is "unqualified")
restored active_value = $62,500
round-trip cost = 240,073 BLNT (~$23,950)
```

After the unwind the victim is fully qualified again — `active_value` returns to
$62,500 exactly — and is still out of the zone. `set_reward_zone` committed the
membership list; nothing re-derives it. The victim earns nothing while out:
`gulp_emissions` reverts with `BadRequest`, against 182,538 BLNT for the
comparable week as a member.

Two things this test also establishes, both of which bound the claim:

- **`require_distribute_run_recently` is not a defence.** It checks that a
  distribution checkpoint is newer than `CHECKPOINT_MAX_AGE_SECONDS`, not that
  the caller is authorized. `distribute` is permissionless, so an attacker
  facing a stale checkpoint refreshes it themselves in the same transaction.
- **Re-entry is cheap while the zone has a free slot.** The test ends by showing
  the victim can walk back in with a single `add_reward` call. The eviction is
  durable only when the reward zone is full, where re-entry requires
  out-weighing an incumbent on the same manipulable weight.

One further consequence fell out of the fixture: its reward zone had exactly one
member, so evicting it emptied the zone and `distribute` then failed outright
with `NoEligibleWeight`. On a populated zone that does not happen — the victim's
share is redirected to the remaining members rather than stalling distribution.

### 3.5.1 Honest economics

Denial alone does not obviously pay: ~$23,950 to deny ~$18,254/week of emissions,
recoverable by the victim with one call, is a break-even of roughly nine days of
the victim not noticing. The version that pays is the one this PoC does *not*
yet cover: a **full** reward zone, where the eviction locks the victim out, and
an **attacker-owned pool** taking the vacated slot so the redirected share is
captured rather than merely denied. That combination should be built before this
is used to size the fix.

## 4. The Dutch curve prices the mispricing back to par

This section replaces the first draft's timing analysis, which was wrong in a
way that changes the finding's severity.

`auction_modifiers` (`pool/src/auctions/math.rs:6`) is the inherited v2 Dutch
curve. For `elapsed <= 200` the **bid modifier is a flat 100%** and the lot
scales `elapsed * 0.005`; only after 200 does the bid decay `100% → 0`. The
first draft read this as "a rational filler waits ~200 ledgers for a full lot at
the full mispriced bid." That is not rational. The bid is already pinned at its
frozen value throughout the ramp, so the auction goes into the money the moment
the partial lot exceeds it, and every searcher watching has the same incentive
to take it then.

`auction_manip_crossover.rs` runs the identical swap-create-unwind, then walks
the ramp:

```
frozen bid real value = $79.07
  elapsed  40: lot $ 40.00  bid $79.07  filler profit $-39.07
  elapsed  60: lot $ 60.00  bid $79.07  filler profit $-19.07
  elapsed  79: lot $ 79.00  bid $79.07  filler profit $ -0.07
  elapsed  80: lot $ 80.00  bid $79.07  filler profit $  0.93
  elapsed 200: lot $200.00  bid $79.07  filler profit $120.93
```

Filling at the crossover (elapsed 80) rather than at 200:

```
credit released  = $80.00
BLNT burned      = $79.07
clearing ratio   = 9883 bps
credit remaining = $120.00 (still protocol credit)
```

**The protocol converted credit into burned BLNT at 0.988:1.** The rest of the
credit is untouched and remains available to a later auction.

This generalizes. At any zero-profit fill the filler pays what they receive:

- ramp (`elapsed <= 200`): pays `bid_real`, receives `lot × elapsed/200` — equal
  at the crossover;
- decay (`elapsed > 200`): pays `bid_real × (400-elapsed)/200`, receives the full
  lot — equal at the crossover.

The frozen quote therefore controls **where on the curve the auction clears, not
the rate at which it clears.** Honest: the $240 ask exceeds the maximum lot
value, so the ramp is never profitable and it clears at elapsed ~233 for ~$200.
Manipulated: clears at elapsed ~80 for ~$79. Both ~1:1. The manipulated auction
just settles a smaller slice and leaves the remainder as credit.

The same argument applies to the interest bid and the bad-debt lot, including
§3.4's whole-tier short-circuit: inflating the lot to the entire tier does not
change the rate a competitive filler clears at, only how early they clear.

### 4.1 What is actually lost

Three residual harms survive, none of them the value transfer the first draft
described:

1. **Throughput, not value.** A $200 lot clears in $80 slices, and a 100%-percent
   fill deletes the auction even though the lot was partial
   (`protocol_fee_auction.rs:169`, `complete = remaining_bid == 0`). The $120
   remainder is below `PROTOCOL_FEE_AUCTION_MINIMUM_VALUE_USDC`, so it cannot be
   re-auctioned until credit accumulates past $200 again.
2. **Griefing.** `new_auction` (`pool/src/contract.rs:677`) has no `require_auth`,
   and `storage::has_auction` blocks any competing honest auction of the same
   type. Cheaply created auctions that clear early and small throttle fee
   conversion without stealing from it.
3. **Thin-filler markets.** If nobody takes a profitable fill for 120 ledgers,
   the manipulator can fill at elapsed 200 and capture the full $121. But this
   scenario undercuts itself: in a market that inattentive, an attacker can
   instead wait for *any honest auction* to decay to elapsed 400, where the bid
   modifier reaches zero and the full lot goes for **free**. That is strictly
   more profitable, needs no capital and pays no swap fees. Manipulation is not
   the marginal risk in the world where manipulation works.

The conclusion the first draft should have reached: on the auction legs this is
a **liveness and accounting-hygiene defect, not a value leak**. The value-leak
claim belongs to §3.3.

## 5. Correction to the v2 comparison

SCAN.md states "v2 had one instance of this (`token_spot_price` in its bad-debt
lot)." v2 actually had **two**, and the pattern is byte-for-byte the same:

- `blend-contracts-v2/pool/src/auctions/backstop_interest_auction.rs:80` —
  `bid_amount = interest_value * 1.2 / token_spot_price`
- `blend-contracts-v2/pool/src/auctions/bad_debt_auction.rs:88` —
  `lot_amount = debt_value * 1.2 / token_spot_price`

and `token_spot_price` in v2 is the same live Comet read
(`blend-contracts-v2/backstop/src/backstop/pool.rs:36-52`).

This matters for how the finding should be triaged: **the class of bug is
inherited, not introduced.** What v3 adds is real but narrower than the scan
implies:

1. a third instance (protocol-fee bid);
2. cross-quoting, so one anchor pool now controls tiers denominated in three
   other assets (§3.3) — the genuinely new amplification;
3. new consumers of the same spot number: activation, pool status, reward-zone
   eligibility and weight;
4. the `available_value <= target_value` whole-tier short-circuit, which in v2
   was the benign `lot_amount = tokens.min(lot_amount)` clamp and is now
   reachable by deliberate deflation.

## 6. Mitigations

The oracle prohibition (`AGENTS.md:181`) rules out the obvious fix. Re-ranked
after §4: the threshold reads, not the auction quotes, are what need protecting.

1. **Harden the threshold reads (recommended, was ranked 2nd).** Store
   `(sequence, pair_reserve, blnt_reserve)` on each `read_comet` and reject a
   valuation whose reserves moved beyond a threshold within the same ledger.
   This is what defends `meets_activation_threshold`, reward-zone entry/exit and
   `pool_spot_blnt_emission_weight` — the §3.3 consumers, which have no clearing
   mechanism and take an irreversible action on a single instantaneous read.
   It is the only listed mitigation that would have stopped §3.5.
   Defeated by an attacker who splits the skew and the read across two ledgers,
   but that exposes them to a full ledger of arbitrage against the skew, which
   is the same economic barrier mitigation (2) relies on.
2. **Revalidate at fill (was ranked 1st, now lower value).** Recompute
   `blnt_price` / tier `value` in the three `fill_*` paths and reject or clamp
   when the stored quote is outside a band (say ±10%) of the live one. Still
   correct, still cheap, and it is what `auction_manip.rs` asserts. But §4 shows
   the Dutch curve already recovers ~1:1 clearing on these legs under
   competitive fillers, so this buys auction throughput and hygiene rather than
   preventing a value leak. Worth doing; not the thing to do first.
3. **Guard the whole-tier short-circuit.** Cap the bad-debt lot at a fraction of
   the tier when `value` has moved sharply, so §3.4 cannot sweep a full tier in
   one event. Independent of the above. Note that §4's clearing argument applies
   here too — a competitive filler on an inflated lot still clears at ~1:1 — so
   this is defence in depth against the thin-market case, not a value fix.
4. **Authenticate or rate-limit `new_auction`.** It has no `require_auth`
   (`pool/src/contract.rs:677`) and `has_auction` blocks competing honest
   auctions, which is the §4.1 griefing vector. Unchanged in priority.
5. **Reuse the `del_tier_auction` escape hatch.** Adding "stored quote diverges
   from live quote" as a fourth early-deletion condition lets anyone cancel a
   mispriced auction without waiting 500 ledgers. A race, not a guarantee, but
   it composes with (2) and (4).

Mitigation (2) alone would have failed every auction PoC in §3. Only mitigation
(1) addresses §3.3 and §3.5, which are now the finding's severity driver.

Two changes worth making regardless of the valuation fix, both surfaced by §3.5
and independent of any price manipulation:

- `remove_reward` and `add_reward` are fully permissionless. Even with honest
  reserves, a pool that legitimately dips below threshold can be evicted by a
  stranger. Requiring auth from the pool, or a guardian, removes the whole class.
- Emptying the reward zone bricks `distribute` with `NoEligibleWeight` until
  somebody re-adds a pool. Distribution should degrade rather than revert.

## 7. Files examined

`backstop/src/backstop/pool.rs`, `backstop/src/contract.rs`,
`backstop/src/emissions/{manager,policy,tier_accounting}.rs`,
`backstop/src/errors.rs`,
`pool/src/auctions/{auction,math,tier_interest,tier_bad_debt,protocol_fee_auction}.rs`,
`pool/src/contract.rs`, `pool/src/pool/status.rs`,
`pool/src/auctions/math.rs`,
`blend-contracts-v2/{backstop/src/backstop/pool.rs,pool/src/auctions/*}`,
`comet-contracts-v2/contracts/src/{c_consts.rs,c_pool/comet.rs}`, `AGENTS.md`.
