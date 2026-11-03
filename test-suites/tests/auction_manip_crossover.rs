#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use pool::{AuctionType, PoolDataKey, Request, RequestType};
use soroban_sdk::{
    contracttype,
    testutils::{Address as _, Ledger},
    vec, Address, Env,
};
use test_suites::{
    create_fixture_with_data,
    test_fixture::{TokenIndex, SCALAR_7},
};

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
struct ProtocolFeeDataView {
    credit: i128,
    carry: i128,
}

fn protocol_fee_data(e: &Env, pool: &Address, asset: &Address) -> ProtocolFeeDataView {
    e.as_contract(pool, || {
        e.storage()
            .persistent()
            .get::<PoolDataKey, soroban_sdk::Map<Address, ProtocolFeeDataView>>(
                &PoolDataKey::ProtocolFees,
            )
            .and_then(|data| data.get(asset.clone()))
            .unwrap_or(ProtocolFeeDataView {
                credit: 0,
                carry: 0,
            })
    })
}

fn set_protocol_fee_data(e: &Env, pool: &Address, asset: &Address, data: ProtocolFeeDataView) {
    e.as_contract(pool, || {
        let mut fees = e
            .storage()
            .persistent()
            .get::<PoolDataKey, soroban_sdk::Map<Address, ProtocolFeeDataView>>(
                &PoolDataKey::ProtocolFees,
            )
            .unwrap_or_else(|| soroban_sdk::Map::new(e));
        fees.set(asset.clone(), data);
        e.storage()
            .persistent()
            .set(&PoolDataKey::ProtocolFees, &fees);
    })
}

/// Regression: a protocol-fee lot must not be sellable below its value.
///
/// Counterweight to `auction_manip.rs`: the Dutch curve prices the frozen
/// quote back to par.
///
/// `auction_modifiers` (`pool/src/auctions/math.rs:6`) holds the bid at 100%
/// while the lot ramps `0 -> 100%` over the first 200 ledgers. A frozen,
/// under-priced bid therefore does not hand a filler a discounted full lot;
/// it just moves the profitable crossover earlier on the curve, where the
/// lot is proportionally smaller. At any competitive fill the protocol
/// converts credit into burned BLNT at ~1:1, and the unsold credit stays in
/// the pool.
///
/// This bounds the economic impact of the frozen protocol-fee quote: the
/// leak is auction throughput, not value, whenever fillers are competitive.
#[test]
fn dutch_curve_clears_manipulated_bid_at_par() {
    let fixture = create_fixture_with_data(false);
    let e = fixture.env.clone();
    let pool = &fixture.pools[0].pool;
    let stable = &fixture.tokens[TokenIndex::STABLE];
    let stable_address = stable.address.clone();
    let blnt = &fixture.tokens[TokenIndex::BLNT];
    let usdc = &fixture.tokens[TokenIndex::USDC];
    let attacker = fixture.users[0].clone();

    // Install an exact $200 protocol-credit lot.
    stable.mint(&pool.address, &1);
    assert_eq!(pool.gulp(&stable_address), 1);
    let mut fee = protocol_fee_data(&e, &pool.address, &stable_address);
    let target_credit = 200 * 10i128.pow(6);
    assert!(fee.credit <= target_credit);
    stable.mint(&pool.address, &(target_credit - fee.credit));
    fee.credit = target_credit;
    set_protocol_fee_data(&e, &pool.address, &stable_address, fee);

    fixture
        .oracle
        .set_price_stable(&vec![&e, 2000_0000000, 1_0000000, 0_1000000, 1_0000000]);

    let honest_price = fixture.backstop.blnt_price();
    let blnt_r0 = fixture.lp.get_balance(&blnt.address);
    let usdc_r0 = fixture.lp.get_balance(&usdc.address);
    std::println!("honest blnt_price = {}", honest_price);
    std::println!("reserves before: blnt={} usdc={}", blnt_r0, usdc_r0);

    // --- single transaction begins ---
    // Step 1: swap USDC in / BLNT out to inflate usdc_reserve and deflate
    // blnt_reserve, i.e. inflate the reported BLNT spot price.
    // Comet caps a single swap at 1/3 of the in-reserve (ErrMaxInRatio), so
    // the attacker compounds several swaps inside the same transaction.
    usdc.mint(&attacker, &(10_000_000 * SCALAR_7));
    let attacker_usdc_before = usdc.balance(&attacker);
    let attacker_blnt_before = blnt.balance(&attacker);
    for _ in 0..4 {
        let reserve = fixture.lp.get_balance(&usdc.address);
        let amt = reserve / 4;
        fixture.lp.swap_exact_amount_in(
            &usdc.address,
            &amt,
            &blnt.address,
            &0,
            &i128::MAX,
            &attacker,
        );
    }
    let skewed_price = fixture.backstop.blnt_price();
    std::println!(
        "skewed blnt_price = {} ({}x)",
        skewed_price,
        skewed_price / honest_price
    );

    // Step 2: create the auction at the skewed quote.
    let auction = pool.new_auction(
        &(AuctionType::ProtocolFeeAuction as u32),
        &fixture.backstop.address,
        &vec![&e],
        &vec![&e, stable_address.clone()],
        &100,
    );
    let skewed_bid = auction.bid.get(blnt.address.clone()).unwrap();

    // Step 3: unwind the swap.
    let mut blnt_left = blnt.balance(&attacker) - attacker_blnt_before;
    while blnt_left > 0 {
        let reserve = fixture.lp.get_balance(&blnt.address);
        let amt = core::cmp::min(blnt_left, reserve / 4);
        if amt == 0 {
            break;
        }
        fixture.lp.swap_exact_amount_in(
            &blnt.address,
            &amt,
            &usdc.address,
            &0,
            &i128::MAX,
            &attacker,
        );
        blnt_left -= amt;
    }
    // --- single transaction ends ---

    let restored_price = fixture.backstop.blnt_price();
    let round_trip_cost = attacker_usdc_before - usdc.balance(&attacker);
    std::println!("restored blnt_price = {}", restored_price);
    std::println!("round-trip USDC cost = {}", round_trip_cost);

    // The honest bid for the same lot, quoted at the restored price. This
    // mirrors what `create_protocol_fee_auction_data` computes:
    //   ceil(ceil(lot_value * 6 / 5) * SCALAR_7 / blnt_price)
    let honest_bid = (target_credit * 10i128.pow(1) * 6 / 5) * SCALAR_7 / restored_price;
    std::println!("skewed bid = {} BLNT", skewed_bid);
    std::println!("honest bid ~= {} BLNT", honest_bid);

    // Value the frozen bid at the restored (real) price.
    let bid_value = skewed_bid * restored_price / SCALAR_7;
    let lot_value_full = target_credit * 10i128.pow(1);
    std::println!("frozen bid real value = {} (7dp USD)", bid_value);

    // Walk the ramp. Bid modifier is 100% for elapsed <= 200, so the filler
    // pays `bid_value` at every point below and takes `elapsed/200` of lot.
    let mut crossover = 0_u32;
    for elapsed in 1..=200_u32 {
        let lot_value = lot_value_full * i128::from(elapsed) * 50_000 / SCALAR_7;
        if lot_value > bid_value {
            crossover = elapsed;
            break;
        }
    }
    assert!(crossover > 0, "no profitable crossover inside the lot ramp");
    std::println!("first profitable elapsed = {}", crossover);
    assert!(
        crossover < 200,
        "a competitive filler acts at elapsed {}, well before the full lot at 200",
        crossover
    );

    // Fill there, as a competitive searcher would.
    let stored = pool.get_auction(
        &(AuctionType::ProtocolFeeAuction as u32),
        &fixture.backstop.address,
    );
    let filler = Address::generate(&e);
    blnt.mint(&filler, &(skewed_bid * 4));
    let filler_blnt_before = blnt.balance(&filler);
    let filler_stable_before = stable.balance(&filler);
    e.ledger().set_sequence_number(stored.block + crossover);
    pool.submit(
        &filler,
        &filler,
        &filler,
        &vec![
            &e,
            Request {
                request_type: RequestType::FillProtocolFeeAuction as u32,
                address: fixture.backstop.address.clone(),
                amount: 100,
            },
        ],
    );

    let blnt_paid = filler_blnt_before - blnt.balance(&filler);
    let stable_taken = stable.balance(&filler) - filler_stable_before;
    let burned_value = blnt_paid * restored_price / SCALAR_7;
    let taken_value = stable_taken * 10i128.pow(1);
    std::println!("credit released  = {} (7dp USD)", taken_value);
    std::println!("BLNT burned      = {} (7dp USD)", burned_value);

    // The whole point: the protocol trades credit for burn at ~par, not at
    // the manipulated 3x discount.
    let ratio_bps = burned_value * 10_000 / taken_value;
    std::println!("clearing ratio   = {} bps", ratio_bps);
    assert!(
        (9_500..=10_500).contains(&ratio_bps),
        "expected ~1:1 clearing, got {} bps ({} burned for {} released)",
        ratio_bps,
        burned_value,
        taken_value
    );

    // The filler took only a slice; the rest is still protocol credit, and
    // the manipulator captured essentially nothing.
    let leftover = protocol_fee_data(&e, &pool.address, &stable_address);
    std::println!("credit remaining = {} (6dp STABLE)", leftover.credit);
    assert_eq!(
        leftover.credit,
        target_credit - stable_taken,
        "unsold credit stays with the protocol"
    );
    assert!(
        leftover.credit > 0,
        "an early crossover fill must leave credit behind"
    );

    // A 100%-percent fill closes the auction even though the lot was
    // partial, so the remainder needs a fresh auction to clear.
    assert!(
        pool.try_get_auction(
            &(AuctionType::ProtocolFeeAuction as u32),
            &fixture.backstop.address
        )
        .is_err(),
        "auction is complete once the full bid is taken"
    );
}
