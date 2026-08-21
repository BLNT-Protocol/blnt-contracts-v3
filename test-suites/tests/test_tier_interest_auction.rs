#![cfg(test)]
#![allow(clippy::zero_prefixed_literal)]

use backstop::BackstopTier;
use pool::{AuctionType, PoolClient, Request, RequestType, ReserveConfig};
use pool_factory::{BackstopAsset as FactoryBackstopAsset, BackstopTierConfig};
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{contracttype, testutils::Ledger, vec, Address, Env, String};
use test_suites::{
    assertions::event_from_end,
    create_fixture_with_data,
    liquidity_pool::LPClient,
    test_fixture::{TestFixture, TokenIndex, SCALAR_12, SCALAR_7},
};

#[derive(Clone)]
#[contracttype]
enum InterestDataKey {
    Reserve(Address),
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
struct InterestReserveState {
    carry: i128,
    first_loss: i128,
    second_loss: i128,
    third_loss: i128,
}

fn interest_reserve_state(e: &Env, pool: &Address, asset: &Address) -> InterestReserveState {
    e.as_contract(pool, || {
        e.storage()
            .persistent()
            .get(&InterestDataKey::Reserve(asset.clone()))
            .unwrap_or(InterestReserveState {
                carry: 0,
                first_loss: 0,
                second_loss: 0,
                third_loss: 0,
            })
    })
}

fn fill_interest(e: &Env, pool: &PoolClient, backstop: &Address, filler: &Address, percent: i128) {
    pool.submit(
        filler,
        filler,
        filler,
        &vec![
            e,
            Request {
                request_type: RequestType::FillInterestAuction as u32,
                address: backstop.clone(),
                amount: percent,
            },
        ],
    );
}

#[test]
fn reserve_loss_exhausts_suppliers_and_cancels_interest_auction_optimized_wasm() {
    use soroban_sdk::{IntoVal, Symbol};

    let fixture = create_fixture_with_data(true);
    let e = fixture.env.clone();
    let pool = &fixture.pools[0].pool;
    let stable = &fixture.tokens[TokenIndex::STABLE];
    let stable_address = stable.address.clone();
    let added_credit = 10_000 * 10i128.pow(6);

    stable.mint(&pool.address, &added_credit);
    assert_eq!(pool.gulp(&stable_address), added_credit);
    fixture
        .oracle
        .set_price_stable(&vec![&e, 2000_0000000, 1_0000000, 0_1000000, 1_0000000]);

    let empty = vec![&e];
    let lot_assets = vec![&e, stable_address.clone()];
    let auction = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &empty,
        &lot_assets,
        &100,
    );
    assert!(auction.lot.get(stable_address.clone()).unwrap() >= 200 * 10i128.pow(6));

    let before = pool.get_reserve(&stable_address).data;
    let supplier_claim = before
        .b_supply
        .fixed_mul_floor(before.b_rate, SCALAR_12)
        .unwrap();
    let backstop_credit_loss = 100 * 10i128.pow(6);
    let loss = supplier_claim + backstop_credit_loss;
    assert!(stable.balance(&pool.address) >= loss);

    stable.burn(&pool.address, &loss);
    assert_eq!(pool.reconcile_loss(&stable_address), loss);
    let reconcile_events = vec![&e, event_from_end(&e, 2), event_from_end(&e, 1)];

    let after = pool.get_reserve(&stable_address).data;
    assert_eq!(after.b_rate, 0);
    assert_eq!(after.b_supply, before.b_supply);
    assert_eq!(after.d_supply, before.d_supply);
    assert_eq!(
        after.backstop_credit,
        before.backstop_credit - backstop_credit_loss
    );
    assert!(pool
        .try_get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        )
        .is_err());
    let pending = interest_reserve_state(&e, &pool.address, &stable_address);
    assert_eq!(
        pending.first_loss + pending.second_loss + pending.third_loss + pending.carry,
        after.backstop_credit
    );

    assert_eq!(
        reconcile_events,
        vec![
            &e,
            (
                pool.address.clone(),
                (
                    Symbol::new(&e, "delete_auction"),
                    AuctionType::InterestAuction as u32,
                    fixture.backstop.address.clone(),
                )
                    .into_val(&e),
                ().into_val(&e),
            ),
            (
                pool.address.clone(),
                (Symbol::new(&e, "reconcile_loss"), stable_address.clone()).into_val(&e),
                (loss, supplier_claim, backstop_credit_loss, before.b_rate,).into_val(&e),
            ),
        ]
    );
    assert_eq!(pool.reconcile_loss(&stable_address), 0);
}

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

    let blnd_xlm_token = fixture
        .backstop
        .backstop_token(&BackstopTier::FirstLoss, &pool_address);
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
        &backstop::BackstopTier::SecondLoss,
        &operator,
        &pool_address,
        &lp_tier_principal,
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::FirstLoss,
        &operator,
        &pool_address,
        &lp_tier_principal,
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::ThirdLoss,
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

    let expected_lots = [800 * SCALAR_7, 600 * SCALAR_7, 400 * SCALAR_7];
    let expected_bids = [768 * SCALAR_7, 576 * SCALAR_7, 480 * SCALAR_7];
    // The BLND:USDC auction fills 300 ledgers into the declining-bid half of
    // the v2 curve. The USDC tier transfers its full bid but credits 99% and
    // reserves 1% for the independent BLND buy-and-burn.
    let realized_payments = [768 * SCALAR_7, 288 * SCALAR_7, 480 * SCALAR_7];
    let realized_tier_gains = [
        realized_payments[0],
        realized_payments[1],
        realized_payments[2] * 99 / 100,
    ];

    let lot_assets = vec![&e, fixture.tokens[TokenIndex::USDC].address.clone()];
    let blnd_usdc_token = fixture
        .backstop
        .backstop_token(&BackstopTier::SecondLoss, &pool_address);
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
            // A nonempty interest bid asserts the selected tier token. This
            // reserve asset is not the canonical first tier and must fail.
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
        // The matching assertion accepts the canonically selected tier and
        // does not supply its bid amount.
        &vec![&e, blnd_xlm_token.clone()],
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
        .try_submit(
            &operator,
            &operator,
            &operator,
            &vec![
                &e,
                Request {
                    request_type: RequestType::FillInterestAuction as u32,
                    address: operator.clone(),
                    amount: 100,
                },
            ],
        )
        .is_err());
    assert!(
        fixture.backstop.deposit(
            &backstop::BackstopTier::SecondLoss,
            &operator,
            &pool_address,
            &SCALAR_7,
        ) > 0
    );
    fixture.backstop.deposit(
        &backstop::BackstopTier::ThirdLoss,
        &operator,
        &pool_address,
        &SCALAR_7,
    );
    let starting_data = fixture.backstop.pool_data(&pool_address);
    let starting_states = [
        starting_data.tiers.get(0).unwrap(),
        starting_data.tiers.get(1).unwrap(),
        starting_data.tiers.get(2).unwrap(),
    ];

    let usdc_token = fixture
        .backstop
        .backstop_token(&BackstopTier::ThirdLoss, &pool_address);
    assert_eq!(
        first
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(expected_lots[0])
    );
    assert_eq!(
        first.bid.get(blnd_xlm_token.clone()),
        Some(expected_bids[0])
    );
    assert_eq!(
        pool.get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        ),
        first
    );

    e.ledger()
        .set_sequence_number(first.block.saturating_add(100));
    let operator_usdc_before = fixture.tokens[TokenIndex::USDC].balance(&operator);
    let tier_assets_before = fixture
        .backstop
        .pool_data(&pool_address)
        .tiers
        .get(0)
        .unwrap()
        .tokens;
    fill_interest(&e, pool, &fixture.backstop.address, &operator, 50);
    assert_eq!(
        fixture.tokens[TokenIndex::USDC].balance(&operator) - operator_usdc_before,
        200 * SCALAR_7
    );
    assert_eq!(
        fixture
            .backstop
            .pool_data(&pool_address)
            .tiers
            .get(0)
            .unwrap()
            .tokens
            - tier_assets_before,
        384 * SCALAR_7
    );
    let remaining_first = pool.get_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
    );
    assert_eq!(
        remaining_first
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(400 * SCALAR_7)
    );
    assert_eq!(
        remaining_first.bid.get(
            fixture
                .backstop
                .backstop_token(&BackstopTier::FirstLoss, &pool_address),
        ),
        Some(384 * SCALAR_7)
    );

    e.ledger()
        .set_sequence_number(first.block.saturating_add(200));
    let operator_usdc_before = fixture.tokens[TokenIndex::USDC].balance(&operator);
    let tier_assets_before = fixture
        .backstop
        .pool_data(&pool_address)
        .tiers
        .get(0)
        .unwrap()
        .tokens;
    fill_interest(&e, pool, &fixture.backstop.address, &operator, 100);
    assert_eq!(
        fixture.tokens[TokenIndex::USDC].balance(&operator) - operator_usdc_before,
        400 * SCALAR_7
    );
    assert_eq!(
        fixture
            .backstop
            .pool_data(&pool_address)
            .tiers
            .get(0)
            .unwrap()
            .tokens
            - tier_assets_before,
        384 * SCALAR_7
    );
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
        second.bid.get(blnd_usdc_token.clone()),
        Some(expected_bids[1])
    );

    e.ledger()
        .set_sequence_number(second.block.saturating_add(300));
    let operator_usdc_before = fixture.tokens[TokenIndex::USDC].balance(&operator);
    let tier_assets_before = fixture
        .backstop
        .pool_data(&pool_address)
        .tiers
        .get(1)
        .unwrap()
        .tokens;
    fill_interest(&e, pool, &fixture.backstop.address, &operator, 100);
    assert_eq!(
        fixture.tokens[TokenIndex::USDC].balance(&operator) - operator_usdc_before,
        expected_lots[1]
    );
    assert_eq!(
        fixture
            .backstop
            .pool_data(&pool_address)
            .tiers
            .get(1)
            .unwrap()
            .tokens
            - tier_assets_before,
        realized_tier_gains[1]
    );

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
    let operator_usdc_before = fixture.tokens[TokenIndex::USDC].balance(&operator);
    let tier_assets_before = fixture
        .backstop
        .pool_data(&pool_address)
        .tiers
        .get(2)
        .unwrap()
        .tokens;
    fill_interest(&e, pool, &fixture.backstop.address, &operator, 100);
    assert_eq!(
        fixture.tokens[TokenIndex::USDC].balance(&operator) - operator_usdc_before,
        expected_lots[2] - realized_payments[2]
    );
    assert_eq!(
        fixture
            .backstop
            .pool_data(&pool_address)
            .tiers
            .get(2)
            .unwrap()
            .tokens
            - tier_assets_before,
        realized_tier_gains[2]
    );
    assert!(pool
        .try_get_auction(
            &(AuctionType::InterestAuction as u32),
            &fixture.backstop.address
        )
        .is_err());

    let pending =
        interest_reserve_state(&e, &pool_address, &fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(pending.second_loss, 0);
    assert_eq!(pending.first_loss, 200 * SCALAR_7);
    assert_eq!(pending.third_loss, 0);
    assert_eq!(pending.carry, 0);
    assert_eq!(
        pool.get_reserve(&fixture.tokens[TokenIndex::USDC].address)
            .data
            .backstop_credit,
        200 * SCALAR_7
    );

    for (index, tier) in [
        BackstopTier::FirstLoss,
        BackstopTier::SecondLoss,
        BackstopTier::ThirdLoss,
    ]
    .iter()
    .enumerate()
    {
        let ending_data = fixture.backstop.pool_data(&pool_address);
        let ending = match tier {
            BackstopTier::SecondLoss => ending_data.tiers.get(1).unwrap(),
            BackstopTier::FirstLoss => ending_data.tiers.get(0).unwrap(),
            BackstopTier::ThirdLoss => ending_data.tiers.get(2).unwrap(),
        };
        assert_eq!(ending.shares, starting_states[index].shares);
        assert_eq!(
            ending.tokens,
            starting_states[index].tokens + realized_tier_gains[index]
        );
    }

    // A fresh checkpoint uses the tiers' appreciated post-fill values. The
    // cyclic cursor returns to BLND:XLM, and stale release must preserve its
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
        interest_reserve_state(&e, &pool_address, &fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(
        stale
            .lot
            .get(fixture.tokens[TokenIndex::USDC].address.clone()),
        Some(pending_before_stale.first_loss)
    );
    assert!(pending_before_stale.first_loss >= 200 * SCALAR_7);

    // Like v2, the pool owns the auction lifecycle, so an active auction does
    // not lock ordinary backstop share operations.
    assert!(
        fixture.backstop.deposit(
            &backstop::BackstopTier::SecondLoss,
            &operator,
            &pool_address,
            &SCALAR_7,
        ) > 0
    );
    fixture.backstop.queue_withdrawal(
        &backstop::BackstopTier::SecondLoss,
        &operator,
        &pool_address,
        &SCALAR_7,
    );
    fixture.backstop.dequeue_withdrawal(
        &backstop::BackstopTier::SecondLoss,
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
        interest_reserve_state(&e, &pool_address, &fixture.tokens[TokenIndex::USDC].address);
    assert_eq!(pending_after_stale, pending_before_stale);
    assert_eq!(
        pending_after_stale.first_loss
            + pending_after_stale.second_loss
            + pending_after_stale.third_loss
            + pending_after_stale.carry,
        760 * SCALAR_7
    );
    assert_eq!(
        pool.get_reserve(&fixture.tokens[TokenIndex::USDC].address)
            .data
            .backstop_credit,
        760 * SCALAR_7
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
    assert!(
        fixture
            .backstop
            .buy_and_burn(&backstop::BackstopAsset::Usdc)
            > 0
    );
    assert_eq!(
        fixture
            .backstop
            .buy_and_burn(&backstop::BackstopAsset::Usdc),
        0
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

#[test]
fn xlm_interest_haircut_uses_blnd_xlm_comet_optimized_wasm() {
    let mut fixture = TestFixture::create(true);
    let e = fixture.env.clone();
    let operator = fixture.users.first().unwrap().clone();
    fixture.backstop_config = vec![
        &e,
        BackstopTierConfig {
            asset: FactoryBackstopAsset::Xlm,
            take_rate_weight: 1,
        },
    ];
    fixture.create_pool(String::from_str(&e, "XLM interest"), 0_1000000, 6, 0);

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
    let pool = &fixture.pools[0].pool;
    let pool_address = pool.address.clone();
    let xlm = &fixture.tokens[TokenIndex::XLM];
    xlm.mint(&operator, &(20_000 * SCALAR_7));
    xlm.approve(
        &operator,
        &fixture.backstop.address,
        &i128::MAX,
        &e.ledger().sequence().saturating_add(10_000),
    );
    fixture.backstop.deposit(
        &BackstopTier::FirstLoss,
        &operator,
        &pool_address,
        &(13_000 * SCALAR_7),
    );
    pool.set_status(&0);

    let credit = 200 * SCALAR_7;
    fixture.tokens[TokenIndex::USDC].mint(&pool_address, &credit);
    assert_eq!(pool.gulp(&fixture.tokens[TokenIndex::USDC].address), credit);
    let auction = pool.new_auction(
        &(AuctionType::InterestAuction as u32),
        &fixture.backstop.address,
        &vec![&e],
        &vec![&e, fixture.tokens[TokenIndex::USDC].address.clone()],
        &100,
    );
    assert_eq!(auction.bid.get(xlm.address.clone()), Some(240 * SCALAR_7));

    e.ledger()
        .set_sequence_number(auction.block.saturating_add(200));
    fill_interest(&e, pool, &fixture.backstop.address, &operator, 100);
    assert!(fixture.backstop.buy_and_burn(&backstop::BackstopAsset::Xlm) > 0);
}
