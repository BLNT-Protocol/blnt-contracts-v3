#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use backstop::BackstopTier;
use pool::{AuctionType, BackstopTier as PoolBackstopTier, ReserveConfig};
use soroban_sdk::{testutils::Ledger, vec, String};
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

    let blnd_xlm_token = fixture.backstop.backstop_token(&BackstopTier::BlndXlm);
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

    // Each fixture LP share is worth $1.25. Use $13,000 of capital in every
    // tier so the 3:4:2 take-rate weights remain directly observable.
    let lp_tier_principal = 10_400 * SCALAR_7;
    let usdc_tier_principal = 13_000 * SCALAR_7;
    fixture.backstop.deposit(
        &backstop::BackstopTier::BlndUsdc,
        &operator,
        &pool_address,
        &lp_tier_principal,
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::BlndXlm,
        &operator,
        &pool_address,
        &lp_tier_principal,
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::Usdc,
        &operator,
        &pool_address,
        &usdc_tier_principal,
    );
    let allowance_expiration = e.ledger().sequence().saturating_add(10_000);
    fixture.lp.approve(
        &operator,
        &fixture.backstop.address,
        &i128::MAX,
        &allowance_expiration,
    );
    blnd_xlm.approve(
        &operator,
        &fixture.backstop.address,
        &i128::MAX,
        &allowance_expiration,
    );
    fixture.tokens[TokenIndex::USDC].approve(
        &operator,
        &fixture.backstop.address,
        &i128::MAX,
        &allowance_expiration,
    );
    pool.set_status(&0);

    let credit = 1_800 * SCALAR_7;
    fixture.tokens[TokenIndex::USDC].mint(&pool_address, &credit);
    assert_eq!(pool.gulp(&fixture.tokens[TokenIndex::USDC].address), credit);
    fixture
        .oracle
        .set_price_stable(&vec![&e, 2000_0000000, 1_0000000, 0_1000000, 1_0000000]);

    let expected_lots = [600 * SCALAR_7, 800 * SCALAR_7, 400 * SCALAR_7];
    let expected_bids = [576 * SCALAR_7, 768 * SCALAR_7, 480 * SCALAR_7];
    // The BLND:XLM auction fills 100 ledgers into the declining-bid half of
    // the v2 curve, so realized donations intentionally diverge from the
    // BLND:XLM=4, BLND:USDC=3, USDC=2 credit-allocation weights.
    let realized_donations = [576 * SCALAR_7, 384 * SCALAR_7, 480 * SCALAR_7];

    let lot_assets = vec![&e, fixture.tokens[TokenIndex::USDC].address.clone()];
    let empty = vec![&e];
    assert!(pool
        .try_new_auction(
            &(AuctionType::InterestAuction as u32),
            &operator,
            &empty,
            &lot_assets,
            &100,
        )
        .is_err());
    assert!(pool
        .try_new_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address,
            &lot_assets,
            &lot_assets,
            &100,
        )
        .is_err());
    assert!(pool
        .try_new_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address,
            &empty,
            &lot_assets,
            &99,
        )
        .is_err());
    assert!(pool
        .try_new_auction(
            &(AuctionType::BadDebtAuction as u32),
            &fixture.backstop.address,
            &empty,
            &lot_assets,
            &100,
        )
        .is_err());
    assert!(pool
        .try_new_auction(&3, &fixture.backstop.address, &empty, &lot_assets, &100)
        .is_err());
    let first = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &empty,
        &lot_assets,
        &100,
    );
    assert!(
        pool.try_new_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address,
            &empty,
            &lot_assets,
            &100,
        )
        .is_err(),
        "one active interest auction must cap pool concurrency at one"
    );
    assert!(pool
        .try_get_auction(&(AuctionType::InterestAuction as u32), &operator)
        .is_err());
    assert!(pool
        .try_del_auction(&(AuctionType::InterestAuction as u32), &operator)
        .is_err());
    assert!(pool
        .try_fill_interest_auction(&PoolBackstopTier::BlndXlm, &operator, &100)
        .is_err());
    assert!(
        fixture.backstop.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &operator,
            &pool_address,
            &SCALAR_7,
        ) > 0
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::Usdc,
        &operator,
        &pool_address,
        &SCALAR_7,
    );
    let starting_data = fixture.backstop.pool_data(&pool_address);
    let starting_states = [
        starting_data.blnd_usdc,
        starting_data.blnd_xlm,
        starting_data.usdc,
    ];

    let blnd_usdc_token = fixture.backstop.backstop_token(&BackstopTier::BlndUsdc);
    let usdc_token = fixture.backstop.backstop_token(&BackstopTier::Usdc);
    assert_eq!(
        first
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(expected_lots[0])
    );
    assert_eq!(first.bid.get(blnd_usdc_token), Some(expected_bids[0]));
    assert_eq!(
        pool.get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        ),
        first
    );

    e.ledger()
        .set_sequence_number(first.block.saturating_add(100));
    let partial = pool.fill_interest_auction(&PoolBackstopTier::BlndUsdc, &operator, &50);
    assert!(!partial.complete);
    assert_eq!(
        partial
            .base_lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(300 * SCALAR_7)
    );
    assert_eq!(
        partial
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(150 * SCALAR_7)
    );
    assert_eq!(
        partial
            .returned_lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(150 * SCALAR_7)
    );
    assert_eq!(partial.base_bid_amount, 288 * SCALAR_7);
    assert_eq!(partial.bid_amount, 288 * SCALAR_7);

    e.ledger()
        .set_sequence_number(first.block.saturating_add(200));
    let blnd_usdc_fill = pool.fill_interest_auction(&PoolBackstopTier::BlndUsdc, &operator, &100);
    assert!(blnd_usdc_fill.complete);
    assert_eq!(
        blnd_usdc_fill
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(300 * SCALAR_7)
    );
    assert_eq!(blnd_usdc_fill.bid_amount, 288 * SCALAR_7);
    assert!(pool
        .try_get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        )
        .is_err());

    let second = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &empty,
        &lot_assets,
        &100,
    );
    assert_eq!(
        second
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(expected_lots[1])
    );
    assert_eq!(
        second.bid.get(blnd_xlm_token.clone()),
        Some(expected_bids[1])
    );

    e.ledger()
        .set_sequence_number(second.block.saturating_add(300));
    let blnd_xlm_fill = pool.fill_interest_auction(&PoolBackstopTier::BlndXlm, &operator, &100);
    assert!(blnd_xlm_fill.complete);
    assert_eq!(
        blnd_xlm_fill
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(expected_lots[1])
    );
    assert_eq!(blnd_xlm_fill.bid_amount, realized_donations[1]);

    let third = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &empty,
        &lot_assets,
        &100,
    );
    assert_eq!(
        third
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(expected_lots[2])
    );
    assert_eq!(third.bid.get(usdc_token), Some(expected_bids[2]));
    e.ledger()
        .set_sequence_number(third.block.saturating_add(200));
    let usdc_fill = pool.fill_interest_auction(&PoolBackstopTier::Usdc, &operator, &100);
    assert!(usdc_fill.complete);
    assert_eq!(
        usdc_fill
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(expected_lots[2])
    );
    assert_eq!(usdc_fill.bid_amount, realized_donations[2]);
    assert!(pool
        .try_get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        )
        .is_err());

    let pending = pool.interest_reserve_state(&fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(pending.blnd_usdc, 150 * SCALAR_7);
    assert_eq!(pending.blnd_xlm, 0);
    assert_eq!(pending.usdc, 0);
    assert_eq!(pending.carry, 0);
    assert_eq!(
        pool.get_reserve(&fixture.tokens[TokenIndex::USDC].address)
            .data
            .backstop_credit,
        150 * SCALAR_7
    );

    for (index, tier) in [
        BackstopTier::BlndUsdc,
        BackstopTier::BlndXlm,
        BackstopTier::Usdc,
    ]
    .iter()
    .enumerate()
    {
        let ending_data = fixture.backstop.pool_data(&pool_address);
        let ending = match tier {
            BackstopTier::BlndUsdc => ending_data.blnd_usdc,
            BackstopTier::BlndXlm => ending_data.blnd_xlm,
            BackstopTier::Usdc => ending_data.usdc,
        };
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
    let stale_assets = vec![&e, fixture.tokens[TokenIndex::USDC].address.clone()];
    let stale = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &empty,
        &stale_assets,
        &100,
    );
    let pending_before_stale =
        pool.interest_reserve_state(&fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(
        stale
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(pending_before_stale.blnd_usdc)
    );
    assert!(pending_before_stale.blnd_usdc >= 200 * SCALAR_7);

    // Like v2, the pool owns the auction lifecycle, so an active auction does
    // not lock ordinary backstop share operations.
    assert!(
        fixture.backstop.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &operator,
            &pool_address,
            &SCALAR_7,
        ) > 0
    );
    fixture.backstop.queue_withdrawal(
        &backstop::BackstopTier::BlndUsdc,
        &operator,
        &pool_address,
        &SCALAR_7,
    );
    fixture.backstop.dequeue_withdrawal(
        &backstop::BackstopTier::BlndUsdc,
        &operator,
        &pool_address,
        &SCALAR_7,
    );

    e.ledger()
        .set_sequence_number(stale.block.saturating_add(500));
    pool.del_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
    );
    assert!(pool
        .try_get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        )
        .is_err());
    let pending_after_stale =
        pool.interest_reserve_state(&fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(pending_after_stale, pending_before_stale);
    assert_eq!(
        pending_after_stale.blnd_usdc
            + pending_after_stale.blnd_xlm
            + pending_after_stale.usdc
            + pending_after_stale.carry,
        710 * SCALAR_7
    );
    assert_eq!(
        pool.get_reserve(&fixture.tokens[TokenIndex::USDC].address)
            .data
            .backstop_credit,
        710 * SCALAR_7
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
    let batch = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &empty,
        &batch_assets,
        &100,
    );
    assert_eq!(batch.lot.len(), 4);
    e.ledger()
        .set_sequence_number(batch.block.saturating_add(500));
    pool.del_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
    );
}

#[test]
fn tier_interest_auctions_cover_all_tiers_native() {
    exercise_tier_interest_auctions(false);
}

#[test]
fn tier_interest_auctions_cover_all_tiers_optimized_wasm() {
    exercise_tier_interest_auctions(true);
}
