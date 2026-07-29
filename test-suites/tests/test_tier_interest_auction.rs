#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use backstop::BackstopTier;
use pool::{BackstopTier as PoolBackstopTier, ReserveConfig};
use soroban_sdk::{testutils::Ledger, vec, BytesN, String};
use test_suites::{
    liquidity_pool::LPClient,
    test_fixture::{TestFixture, TokenIndex, SCALAR_7},
};

fn exercise_tier_interest_auctions(wasm: bool) {
    let mut fixture = TestFixture::create(wasm);
    let e = fixture.env.clone();
    // The native mock factory's generated pool address can coincide with its
    // admin in the host address generator, so use an independent account for
    // deposits and fills.
    let operator = fixture.users.first().unwrap().clone();

    fixture.create_pool(String::from_str(&e, "Tier interest"), 0_1000000, 6, 0);

    let reserve_config = ReserveConfig {
        c_factor: 0_9000000,
        decimals: 7,
        index: 0,
        l_factor: 0_9000000,
        max_util: 1_0000000,
        reactivity: 0,
        r_base: 0_1000000,
        r_one: 0,
        r_two: 0,
        r_three: 0,
        util: 0_5000000,
        supply_cap: i64::MAX as i128,
        enabled: true,
    };
    fixture.create_pool_reserve(0, TokenIndex::USDC, &reserve_config);
    let mut weth_config = reserve_config.clone();
    weth_config.decimals = 9;
    fixture.create_pool_reserve(0, TokenIndex::WETH, &weth_config);
    fixture.create_pool_reserve(0, TokenIndex::XLM, &reserve_config);
    let mut stable_config = reserve_config.clone();
    stable_config.decimals = 6;
    fixture.create_pool_reserve(0, TokenIndex::STABLE, &stable_config);
    let pool = &fixture.pools[0].pool;
    let pool_address = pool.address.clone();

    let blnd_xlm_token = fixture.backstop.tier_token(&BackstopTier::BlndXlm);
    let blnd_xlm = LPClient::new(&e, &blnd_xlm_token);
    fixture.tokens[TokenIndex::BLND].mint(&operator, &(500_000 * SCALAR_7));
    fixture.tokens[TokenIndex::USDC].mint(&operator, &(50_000 * SCALAR_7));
    fixture.tokens[TokenIndex::XLM].mint(&operator, &(50_000 * SCALAR_7));
    fixture.lp.join_pool(
        &(20_000 * SCALAR_7),
        &vec![&e, i128::MAX, i128::MAX],
        &operator,
    );
    blnd_xlm.join_pool(
        &(20_000 * SCALAR_7),
        &vec![&e, i128::MAX, i128::MAX],
        &operator,
    );

    let tier_principal = 13_000 * SCALAR_7;
    fixture
        .backstop
        .deposit_blnd_usdc(&operator, &pool_address, &tier_principal);
    fixture
        .backstop
        .deposit_blnd_xlm(&operator, &pool_address, &tier_principal);
    fixture
        .backstop
        .deposit_usdc(&operator, &pool_address, &tier_principal);
    pool.set_status(&0);

    let credit = 700 * SCALAR_7;
    fixture.tokens[TokenIndex::USDC].mint(&pool_address, &credit);
    assert_eq!(pool.gulp(&fixture.tokens[TokenIndex::USDC].address), credit);
    fixture
        .oracle
        .set_price_stable(&vec![&e, 2000_0000000, 1_0000000, 0_1000000, 1_0000000]);

    let expected = [
        (PoolBackstopTier::BlndUsdc, 250 * SCALAR_7, 300 * SCALAR_7),
        (PoolBackstopTier::BlndXlm, 250 * SCALAR_7, 300 * SCALAR_7),
        (PoolBackstopTier::Usdc, 200 * SCALAR_7, 240 * SCALAR_7),
    ];
    // The BLND:XLM auction fills 100 ledgers into the declining-bid half of
    // the v2 curve, so realized donations intentionally diverge from 5:5:4.
    let realized_donations = [300 * SCALAR_7, 150 * SCALAR_7, 240 * SCALAR_7];
    let starting_states = [
        fixture
            .backstop
            .pool_tier_state(&BackstopTier::BlndUsdc, &pool_address),
        fixture
            .backstop
            .pool_tier_state(&BackstopTier::BlndXlm, &pool_address),
        fixture
            .backstop
            .pool_tier_state(&BackstopTier::Usdc, &pool_address),
    ];

    for (index, (tier, lot, bid)) in expected.iter().enumerate() {
        let auction_id = BytesN::from_array(&e, &[index as u8 + 1; 32]);
        let auction = pool.new_interest_auction(
            &auction_id,
            &vec![&e, fixture.tokens[TokenIndex::USDC].address.clone()],
        );
        assert_eq!(auction.tier, *tier);
        assert_eq!(
            auction
                .auction
                .lot
                .get(fixture.tokens[TokenIndex::USDC].address.clone()),
            Some(*lot)
        );
        assert_eq!(auction.auction.bid.values().get(0), Some(*bid));

        if index == 0 {
            e.ledger()
                .set_sequence_number(auction.auction.block.saturating_add(100));
            let partial = pool.fill_interest_auction(&operator, &50);
            assert!(!partial.complete);
            assert_eq!(
                partial
                    .base_lot
                    .get(fixture.tokens[TokenIndex::USDC].address.clone()),
                Some(125 * SCALAR_7)
            );
            assert_eq!(
                partial
                    .lot
                    .get(fixture.tokens[TokenIndex::USDC].address.clone()),
                Some(62_5000000)
            );
            assert_eq!(
                partial
                    .returned_lot
                    .get(fixture.tokens[TokenIndex::USDC].address.clone()),
                Some(62_5000000)
            );
            assert_eq!(partial.base_bid_amount, 150 * SCALAR_7);
            assert_eq!(partial.bid_amount, 150 * SCALAR_7);
        }

        e.ledger()
            .set_sequence_number(auction.auction.block.saturating_add(if index == 1 {
                300
            } else {
                200
            }));
        let fill = pool.fill_interest_auction(&operator, &100);
        assert!(fill.complete);
        assert_eq!(
            fill.lot
                .get(fixture.tokens[TokenIndex::USDC].address.clone()),
            Some(if index == 0 { 125 * SCALAR_7 } else { *lot })
        );
        assert_eq!(
            fill.bid_amount,
            if index == 0 {
                150 * SCALAR_7
            } else {
                realized_donations[index]
            }
        );
        assert!(pool.try_get_interest_auction().is_err());
    }

    let pending = pool.interest_reserve_state(&fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(pending.blnd_usdc, 62_5000000);
    assert_eq!(pending.blnd_xlm, 0);
    assert_eq!(pending.usdc, 0);
    assert_eq!(pending.carry, 0);
    assert_eq!(
        pool.get_reserve(&fixture.tokens[TokenIndex::USDC].address)
            .data
            .backstop_credit,
        62_5000000
    );

    for (index, tier) in [
        BackstopTier::BlndUsdc,
        BackstopTier::BlndXlm,
        BackstopTier::Usdc,
    ]
    .iter()
    .enumerate()
    {
        let ending = fixture.backstop.pool_tier_state(tier, &pool_address);
        assert_eq!(ending.shares, starting_states[index].shares);
        assert_eq!(
            ending.assets,
            starting_states[index].assets + realized_donations[index]
        );
    }

    // A fresh checkpoint uses the tiers' appreciated post-fill values. The
    // cyclic cursor returns to BLND:USDC, and stale release must preserve its
    // exact weighted pending amount.
    let additional_credit = 560 * SCALAR_7;
    fixture.tokens[TokenIndex::USDC].mint(&pool_address, &additional_credit);
    assert_eq!(
        pool.gulp(&fixture.tokens[TokenIndex::USDC].address),
        additional_credit
    );
    let stale_id = BytesN::from_array(&e, &[9; 32]);
    let stale = pool.new_interest_auction(
        &stale_id,
        &vec![&e, fixture.tokens[TokenIndex::USDC].address.clone()],
    );
    assert_eq!(stale.tier, PoolBackstopTier::BlndUsdc);
    let pending_before_stale =
        pool.interest_reserve_state(&fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(stale.lot_value, pending_before_stale.blnd_usdc);
    assert!(stale.lot_value >= 200 * SCALAR_7);

    // The committed tier blocks share-changing deposits, while queue/dequeue
    // remain available because they do not change total shares.
    assert!(fixture
        .backstop
        .try_deposit_blnd_usdc(&operator, &pool_address, &SCALAR_7)
        .is_err());
    fixture
        .backstop
        .queue_blnd_usdc_withdrawal(&operator, &pool_address, &SCALAR_7);
    fixture
        .backstop
        .dequeue_blnd_usdc_withdrawal(&operator, &pool_address, &SCALAR_7);

    e.ledger()
        .set_sequence_number(stale.auction.block.saturating_add(500));
    pool.delete_stale_interest_auction();
    assert!(pool.try_get_interest_auction().is_err());
    assert!(fixture
        .backstop
        .interest_commitment(&pool_address, &stale_id)
        .is_none());
    let pending_after_stale =
        pool.interest_reserve_state(&fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(pending_after_stale, pending_before_stale);
    assert_eq!(
        pending_after_stale.blnd_usdc
            + pending_after_stale.blnd_xlm
            + pending_after_stale.usdc
            + pending_after_stale.carry,
        622_5000000
    );
    assert_eq!(
        pool.get_reserve(&fixture.tokens[TokenIndex::USDC].address)
            .data
            .backstop_credit,
        622_5000000
    );

    // Exercise the maximum four-reserve batch against both native and
    // optimized-WASM contracts. Each reserve contributes exactly $200 of
    // fresh credit at the fixture's oracle prices.
    let batch_credits = [
        (TokenIndex::USDC, 200 * SCALAR_7),
        (TokenIndex::WETH, 100_000_000),
        (TokenIndex::XLM, 2_000 * SCALAR_7),
        (TokenIndex::STABLE, 200_000_000),
    ];
    let mut batch_assets = soroban_sdk::Vec::new(&e);
    for (token_index, amount) in batch_credits {
        fixture.tokens[token_index].mint(&pool_address, &amount);
        assert_eq!(pool.gulp(&fixture.tokens[token_index].address), amount);
        batch_assets.push_back(fixture.tokens[token_index].address.clone());
    }
    let batch = pool.new_interest_auction(&BytesN::from_array(&e, &[10; 32]), &batch_assets);
    assert_eq!(batch.auction.lot.len(), 4);
    e.ledger()
        .set_sequence_number(batch.auction.block.saturating_add(500));
    pool.delete_stale_interest_auction();
}

#[test]
fn tier_interest_auctions_cover_all_tiers_native() {
    exercise_tier_interest_auctions(false);
}

#[test]
fn tier_interest_auctions_cover_all_tiers_optimized_wasm() {
    exercise_tier_interest_auctions(true);
}
