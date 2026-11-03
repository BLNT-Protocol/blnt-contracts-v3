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

**Disposition.** §3.1 (protocol-fee auction) is **accepted, WONTFIX** — see §7.
§3.3/§3.5 (threshold and reward-zone consumers) remain open, and their fix lives
in `BUG2_REWARD_ZONE_AUTH.md`.

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

Seven tests against the real `comet.wasm`, in `test-suites/tests/`:

| test | shows | status |
|---|---|---|
| `auction_manip.rs` | frozen quote survives the unwind (§3.1) | passes — canary pinning the accepted discount |
| `auction_manip_cost.rs` | inflation cost curve (§3.2) | passes |
| `auction_manip_tiers.rs` | anchor moves untouched tiers (§3.3) | passes |
| `auction_manip_deflate.rs` | deflation cost curve (§3.4) | passes |
| `auction_manip_crossover.rs` | Dutch curve clears at par (§4) | passes |
| `auction_manip_eviction.rs` | permissionless reward-zone eviction (§3.5) | passes |
| `usdc_only_backstop_immunity.rs` | BLNT-free backstops are unaffected (§3.6) | passes |

All seven pass. `auction_manip.rs` originally asserted the property a fix would
provide and was therefore red; with §7's WONTFIX it now pins the *observed*
discount to a 30-40% band instead. It trips if the discount deepens materially,
and equally if the quoting path is ever changed — a fix would push the bid to
~100% of honest value and break the upper bound, which is the intended signal
that this canary and the WONTFIX both need revisiting. Run with
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
end-to-end; §3.6 bounds which pools are exposed at all.

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
- **The eviction being unauthenticated is not established by this test.** The
  fixture runs under `mock_all_auths()` (`test-suites/src/test_fixture.rs:67`),
  which masks a missing `require_auth`. That `remove_reward` needs no
  authorization is proved separately, with a positive control, in
  `reward_zone_auth.rs` — written up as **BUG2**. This test shows only that a
  manipulated valuation makes a qualified pool evictable.
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

Note also that the eviction does not depend on this finding at all: BUG2 shows
the same removal succeeds on any honest threshold crossing, with no manipulation.
The fix for this leg therefore belongs to BUG2's hysteresis mitigation, not to
anything in §6 below.

### 3.6 Scope limit: a backstop with no BLNT-bearing tier is immune

`usdc_only_backstop_immunity.rs`. Every valuation leg above requires the victim
pool to hold a tier whose value derives from Comet. `build_pool_valuation` sets
`needs_anchor` / `needs_target` only for `BlntUsdc`, `BlntXlm` and `Xlm`
(`backstop/src/backstop/pool.rs:339-342`) and reads Comet lazily behind
`needs_anchor.then(|| read_comet(..))` (`:394`). `BackstopAsset::Usdc` takes the
`unit_pool_tier_valuation` pass-through (`:498`) — the tier is worth its token
count, full stop.

A pool configured with a single USDC tier, run through the same anchor deflation
that guts the stock fixture pool:

```
usdc-only active_value: 500000000000 -> 500000000000   (unchanged, exactly)
mixed     active_value: 625000000000 -> 105362278156   (16% of before)
blnt_price:                  1000000 -> 107890         (10% of before)
```

Not "barely moved" — bit-for-bit identical, because no Comet read enters the
valuation at all. So §3.3 and §3.5, and the manipulation route into BUG2, all
require the victim to hold a BLNT-bearing backstop tier. For a pool configured
`Usdc`-only, `active_value` changes only when tokens actually move.

**Two limits on this immunity.**

First, it does not cover the protocol-fee auction. `blnt_price`
(`backstop/src/backstop/pool.rs:446`) is a global read of the anchor with no pool
argument, and every pool's protocol-fee bid is denominated in BLNT regardless of
what its backstop holds. §3.1 therefore applies to a USDC-only pool exactly as it
does to any other. The test asserts this explicitly rather than leaving it
implied.

Second — established by reading, not by this test — such a pool has **zero BLNT
emission weight**. `pool_weight` (`backstop/src/emissions/tier_accounting.rs:18`)
resolves to `pool_spot_blnt_emission_weight` (`policy.rs:86`), which sums
underlying BLNT across the BLNT:USDC and BLNT:XLM tiers only; with neither
present it is 0 on both migration branches. The immunity in §3.3/§3.5 is
therefore somewhat academic: a backstop with no BLNT exposure is also outside the
emission system those findings are about. It is a real mitigation for a pool that
wants threshold stability, not a free lunch.

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

1. ~~**Harden the threshold reads with a same-ledger reserve-move guard.**~~
   **Does not work — withdrawn.** The idea was to snapshot
   `(sequence, pair_reserve, blnt_reserve)` on each `read_comet` and reject a
   valuation whose reserves moved within the same ledger. It has no usable
   reference point. The attack is `[swap, act, unwind]` inside one transaction
   and the swap comes *first*, so the first `read_comet` of that ledger — the
   read that would establish the baseline — is already the manipulated one. The
   guard compares a skewed value against itself and passes. This applies to the
   auction legs and to §3.3/§3.5 alike.

   Making it work needs a reference the attacker cannot set in the same
   transaction, i.e. the *previous* ledger's reserves. That is a lagged
   reference, and it collides with `AGENTS.md:97-100` ("Backstop value comes
   only from current canonical Comet v2 reserves… Backstop valuation has no
   oracle input"); it puts a storage write in `read_comet`, which is reached
   from the unauthenticated `pool_data` and `blnt_price` views; and it
   false-positives on any legitimate large LP join or exit.
2. **Revalidate at fill — the only valuation fix that works, and not
   recommended now.** Compare the stored quote against a freshly computed one at
   fill: two current reads separated by 80-200 ledgers, no oracle, no new
   persisted state, spec-clean. It is the property `auction_manip.rs` asserted
   before the WONTFIX in §7.

   Cost is the reason to defer it. `AuctionData` is `{bid, lot, block}` with
   nowhere to record the price a quote was struck at, so a cheap
   stored-price-vs-live-price comparison is not available; the bid has to be
   re-derived at fill from the lot, its oracle prices and the live `blnt_price`.
   That means lifting `protocol_fee_auction.rs:41-75` into a helper shared by
   create and fill, then repeating for the interest and bad-debt paths — roughly
   100-150 lines across three fill paths plus tests. `pool.wasm` is 97,038 bytes
   against the 120,000-byte guard, so size is not the constraint.

   Set against §4, that spend converts a throughput-and-griefing defect into
   nothing. **Not worth doing now.**
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

**Decision: accepted and documented, not fixed.** The withdrawal of (1) and the
cost of (2), taken with §4's result that competitive fillers already recover
~1:1 clearing, leave no auction-leg change worth making. The §3.3/§3.5 threshold
consumers are real, but their practical fix is BUG2's hysteresis mitigation,
which needs no valuation change at all.

See §7 for the accepted-risk record and what would reopen it.

Two changes worth making regardless of the valuation fix, both surfaced by §3.5
and independent of any price manipulation:

- `remove_reward` and `add_reward` are fully permissionless. Even with honest
  reserves, a pool that legitimately dips below threshold can be evicted by a
  stranger. Requiring auth from the pool, or a guardian, removes the whole class.
- Emptying the reward zone bricks `distribute` with `NoEligibleWeight` until
  somebody re-adds a pool. Distribution should degrade rather than revert.

The permission half of the first bullet is written up separately as **BUG2**,
with its own PoC and mitigations.

## 7. Accepted risk (WONTFIX)

The §3.1 protocol-fee mispricing is **accepted, not fixed**, on this reasoning:

> At worst this creates an auction that opens asking too few backstop tokens. It
> does not let anyone buy the lot below value. The bid modifier is pinned at 100%
> while the lot ramps, so a competitive filler takes the auction early at a
> proportionally smaller lot, and the protocol still clears at ~1:1. The filler
> chooses *when* to fill, not how much to pay.

Evidence: §4 and `auction_manip_crossover.rs` (0.988:1 clearing at elapsed 80).
Cost of the alternative: §6 mitigation (2), ~100-150 lines across three fill
paths.

`auction_manip.rs` is retained as the canary for this decision, pinning the
discount to 30-40% of honest value (observed: 32%).

**What would reopen this:**

- The canary's band breaking in either direction — a deeper discount, or a
  change to the quoting path.
- Evidence that the filler market is not competitive in practice. §4.1 assumes
  searchers take a profitable fill within ~120 ledgers; if real fills cluster
  near elapsed 200, the protocol does lose the full discount.
- Any new consumer of a stored spot quote that is *not* settled through a Dutch
  auction. The whole argument rests on the clearing mechanism; a consumer
  without one inherits §3.3's severity instead, not §3.1's.

Note this record covers §3.1 only. §3.3/§3.5 are not accepted here — their fix
lives in BUG2.

## 8. Files examined

`backstop/src/backstop/pool.rs`, `backstop/src/contract.rs`,
`backstop/src/emissions/{manager,policy,tier_accounting}.rs`,
`backstop/src/errors.rs`,
`pool/src/auctions/{auction,math,tier_interest,tier_bad_debt,protocol_fee_auction}.rs`,
`pool/src/contract.rs`, `pool/src/pool/status.rs`,
`pool/src/auctions/math.rs`,
`blend-contracts-v2/{backstop/src/backstop/pool.rs,pool/src/auctions/*}`,
`comet-contracts-v2/contracts/src/{c_consts.rs,c_pool/comet.rs}`, `AGENTS.md`.
