#![cfg(test)]

use backstop::BackstopTier as BackstopContractTier;
use pool::{
    BackstopLossState, BackstopTier, PoolDataKey, Positions, Request, RequestType, ReserveData,
};
use sep_40_oracle::testutils::Asset;
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{
    contracttype, map,
    testutils::{Address as _, Ledger},
    vec, Address, BytesN, Map, Symbol,
};
use test_suites::{
    assertions::assert_approx_eq_abs,
    backstop::set_mock_backstop_valuation_version,
    create_fixture_with_data,
    liquidity_pool::LPClient,
    test_fixture::{TokenIndex, SCALAR_12, SCALAR_7},
};

#[derive(Clone)]
#[contracttype]
struct CanonicalBackstopLossRecords {
    committed_losses: Map<BytesN<32>, bool>,
    liabilities: Map<Address, i128>,
    unresolved_bad_debt: Map<Address, i128>,
}

#[test]
fn test_wasm_prepares_and_releases_bad_debt_lot() {
    let fixture = create_fixture_with_data(true);
    let pool_fixture = &fixture.pools[0];
    let stable = fixture.tokens[TokenIndex::STABLE].address.clone();
    let stable_index = pool_fixture.reserves[&TokenIndex::STABLE];
    let debt = 50 * 10i128.pow(6);
    let positions = Positions {
        liabilities: map![&fixture.env, (stable_index, debt)],
        collateral: map![&fixture.env],
        supply: map![&fixture.env],
    };
    let records = CanonicalBackstopLossRecords {
        committed_losses: map![&fixture.env],
        liabilities: map![&fixture.env, (stable.clone(), debt)],
        unresolved_bad_debt: map![&fixture.env],
    };

    fixture.env.as_contract(&pool_fixture.pool.address, || {
        fixture.env.storage().persistent().set(
            &PoolDataKey::Positions(fixture.backstop.address.clone()),
            &positions,
        );
        fixture
            .env
            .storage()
            .instance()
            .set(&Symbol::new(&fixture.env, "LossRec"), &records);
    });

    let auction_id = BytesN::from_array(&fixture.env, &[9; 32]);
    let auction = pool_fixture
        .pool
        .new_bad_debt_auction(&auction_id, &vec![&fixture.env, stable.clone()]);
    assert_eq!(auction.auction_id, auction_id);
    assert_eq!(auction.lot_quote.tier, BackstopTier::BlndUsdc);
    assert!(auction.lot_quote.lot_amount > 0);
    assert_eq!(
        fixture
            .backstop
            .pool_bad_debt_commitment_count(&pool_fixture.pool.address),
        1
    );
    assert_eq!(
        pool_fixture.pool.backstop_loss_state(),
        BackstopLossState {
            committed_loss_entries: 1,
            liability_entries: 1,
            unresolved_bad_debt_entries: 0,
        }
    );

    fixture
        .env
        .ledger()
        .set_sequence_number(auction.block + 500);
    pool_fixture.pool.delete_stale_bad_debt_auction();
    assert!(pool_fixture.pool.try_get_bad_debt_auction().is_err());
    assert_eq!(
        fixture
            .backstop
            .pool_bad_debt_commitment_count(&pool_fixture.pool.address),
        0
    );
    assert_eq!(
        pool_fixture.pool.backstop_loss_state(),
        BackstopLossState {
            committed_loss_entries: 0,
            liability_entries: 1,
            unresolved_bad_debt_entries: 0,
        }
    );
}

#[test]
fn test_wasm_partially_and_completely_fills_bad_debt_lot() {
    let fixture = create_fixture_with_data(true);
    let pool_fixture = &fixture.pools[0];
    let filler = fixture.users[0].clone();
    let unhealthy_filler = Address::generate(&fixture.env);
    let stable = fixture.tokens[TokenIndex::STABLE].address.clone();
    let stable_index = pool_fixture.reserves[&TokenIndex::STABLE];
    let debt = 50 * 10i128.pow(6);
    let positions = Positions {
        liabilities: map![&fixture.env, (stable_index, debt)],
        collateral: map![&fixture.env],
        supply: map![&fixture.env],
    };
    let records = CanonicalBackstopLossRecords {
        committed_losses: map![&fixture.env],
        liabilities: map![&fixture.env, (stable.clone(), debt)],
        unresolved_bad_debt: map![&fixture.env],
    };

    fixture.env.as_contract(&pool_fixture.pool.address, || {
        fixture.env.storage().persistent().set(
            &PoolDataKey::Positions(fixture.backstop.address.clone()),
            &positions,
        );
        fixture
            .env
            .storage()
            .instance()
            .set(&Symbol::new(&fixture.env, "LossRec"), &records);
    });

    let auction_id = BytesN::from_array(&fixture.env, &[10; 32]);
    let auction = pool_fixture
        .pool
        .new_bad_debt_auction(&auction_id, &vec![&fixture.env, stable.clone()]);
    let filler_positions_before = pool_fixture.pool.get_positions(&filler);
    let filler_lp_before = fixture.lp.balance(&filler);
    let tier_assets_before = fixture
        .backstop
        .pool_tier_state(&BackstopContractTier::BlndUsdc, &pool_fixture.pool.address)
        .assets;

    fixture
        .env
        .ledger()
        .set_sequence_number(auction.block + 100);
    fixture.backstop.distribute();
    assert!(pool_fixture
        .pool
        .try_fill_bad_debt_auction(&unhealthy_filler, &50)
        .is_err());
    assert_eq!(pool_fixture.pool.get_bad_debt_auction(), auction);
    assert_eq!(fixture.lp.balance(&unhealthy_filler), 0);

    let first = pool_fixture.pool.fill_bad_debt_auction(&filler, &50);
    let first_bid = first.bid.get(stable.clone()).unwrap();
    assert!(!first.complete);
    assert_eq!(first_bid, (debt + 1) / 2);
    assert_eq!(first.base_lot_amount, auction.lot_quote.lot_amount / 2);
    assert_eq!(first.lot_amount, first.base_lot_amount / 2);
    assert_eq!(
        fixture.lp.balance(&filler),
        filler_lp_before + first.lot_amount
    );
    assert_eq!(
        fixture
            .backstop
            .pool_tier_state(&BackstopContractTier::BlndUsdc, &pool_fixture.pool.address,)
            .assets,
        tier_assets_before - first.lot_amount
    );
    assert_eq!(
        pool_fixture
            .pool
            .get_positions(&fixture.backstop.address)
            .liabilities
            .get(stable_index),
        Some(debt - first_bid)
    );
    assert_eq!(
        pool_fixture
            .pool
            .get_positions(&filler)
            .liabilities
            .get(stable_index),
        Some(
            filler_positions_before
                .liabilities
                .get(stable_index)
                .unwrap_or(0)
                + first_bid
        )
    );
    assert_eq!(
        fixture
            .backstop
            .bad_debt_commitment(&pool_fixture.pool.address, &auction_id)
            .unwrap()
            .lot_amount,
        auction.lot_quote.lot_amount - first.base_lot_amount
    );

    fixture
        .env
        .ledger()
        .set_sequence_number(auction.block + 300);
    let second = pool_fixture.pool.fill_bad_debt_auction(&filler, &100);
    assert!(second.complete);
    assert_eq!(
        fixture.lp.balance(&filler),
        filler_lp_before + first.lot_amount + second.lot_amount
    );
    assert!(pool_fixture.pool.try_get_bad_debt_auction().is_err());
    assert_eq!(
        fixture
            .backstop
            .pool_bad_debt_commitment_count(&pool_fixture.pool.address),
        0
    );
    let remaining_debt = pool_fixture
        .pool
        .get_positions(&fixture.backstop.address)
        .liabilities
        .get(stable_index)
        .unwrap();
    assert!(remaining_debt > 0);
    assert!(!pool_fixture
        .pool
        .backstop_withdrawal_allowed(&fixture.backstop.address));

    let continuation_id = BytesN::from_array(&fixture.env, &[11; 32]);
    let continuation = pool_fixture
        .pool
        .continue_bad_debt_resolution(&continuation_id);
    assert!(continuation.auction_created);
    assert!(continuation.defaulted.is_empty());
    let continued_auction = pool_fixture.pool.get_bad_debt_auction();
    assert_eq!(continued_auction.auction_id, continuation_id);
    assert_eq!(continued_auction.lot_quote.tier, BackstopTier::BlndUsdc);
    assert_eq!(
        continued_auction.bid.get(stable.clone()),
        Some(remaining_debt)
    );

    fixture
        .env
        .ledger()
        .set_sequence_number(continued_auction.block + 200);
    let third = pool_fixture.pool.fill_bad_debt_auction(&filler, &100);
    assert!(third.complete);
    assert_eq!(
        fixture.lp.balance(&filler),
        filler_lp_before + first.lot_amount + second.lot_amount + third.lot_amount
    );
    assert!(pool_fixture
        .pool
        .get_positions(&fixture.backstop.address)
        .liabilities
        .is_empty());
    assert_eq!(
        pool_fixture.pool.backstop_loss_state(),
        BackstopLossState {
            committed_loss_entries: 0,
            liability_entries: 0,
            unresolved_bad_debt_entries: 0,
        }
    );
    assert!(pool_fixture
        .pool
        .backstop_withdrawal_allowed(&fixture.backstop.address));
}

#[test]
fn test_wasm_defaults_suppliers_only_after_verified_tier_exhaustion() {
    let fixture = create_fixture_with_data(true);
    let pool_fixture = &fixture.pools[0];
    let frodo = fixture.users[0].clone();
    let stable = fixture.tokens[TokenIndex::STABLE].address.clone();
    let stable_index = pool_fixture.reserves[&TokenIndex::STABLE];
    let deposited_shares = 50_000 * SCALAR_7;

    fixture.backstop.distribute();
    fixture.backstop.queue_blnd_usdc_withdrawal(
        &frodo,
        &pool_fixture.pool.address,
        &deposited_shares,
    );
    fixture.jump(17 * 24 * 60 * 60 + 1);
    fixture.backstop.distribute();
    fixture.backstop.withdraw_blnd_usdc(
        &frodo,
        &pool_fixture.pool.address,
        &deposited_shares,
        &frodo,
    );
    assert_eq!(
        fixture
            .backstop
            .pool_tier_state(&BackstopContractTier::BlndUsdc, &pool_fixture.pool.address)
            .assets,
        0
    );

    let debt = 50 * 10i128.pow(6);
    let mut frodo_positions = pool_fixture.pool.get_positions(&frodo);
    let frodo_debt = frodo_positions.liabilities.get(stable_index).unwrap();
    frodo_positions
        .liabilities
        .set(stable_index, frodo_debt - debt);
    let backstop_positions = Positions {
        liabilities: map![&fixture.env, (stable_index, debt)],
        collateral: map![&fixture.env],
        supply: map![&fixture.env],
    };
    let records = CanonicalBackstopLossRecords {
        committed_losses: map![&fixture.env],
        liabilities: map![&fixture.env, (stable.clone(), debt)],
        unresolved_bad_debt: map![&fixture.env],
    };
    fixture.env.as_contract(&pool_fixture.pool.address, || {
        fixture
            .env
            .storage()
            .persistent()
            .set(&PoolDataKey::Positions(frodo.clone()), &frodo_positions);
        fixture.env.storage().persistent().set(
            &PoolDataKey::Positions(fixture.backstop.address.clone()),
            &backstop_positions,
        );
        fixture
            .env
            .storage()
            .instance()
            .set(&Symbol::new(&fixture.env, "LossRec"), &records);
    });

    let reserve_before_failure = fixture.read_reserve_data(0, TokenIndex::STABLE);
    let valuation = fixture.backstop.backstop_valuation();
    set_mock_backstop_valuation_version(&fixture.env, &valuation, 2);
    let auction_id = BytesN::from_array(&fixture.env, &[12; 32]);
    assert!(pool_fixture
        .pool
        .try_continue_bad_debt_resolution(&auction_id)
        .is_err());
    let positions_after_failure = pool_fixture.pool.get_positions(&fixture.backstop.address);
    assert_eq!(
        positions_after_failure.liabilities,
        backstop_positions.liabilities
    );
    assert_eq!(
        positions_after_failure.collateral,
        backstop_positions.collateral
    );
    assert_eq!(positions_after_failure.supply, backstop_positions.supply);
    let reserve_after_failure = fixture.read_reserve_data(0, TokenIndex::STABLE);
    assert_eq!(
        reserve_after_failure.d_supply,
        reserve_before_failure.d_supply
    );
    assert_eq!(
        reserve_after_failure.b_supply,
        reserve_before_failure.b_supply
    );
    assert_eq!(reserve_after_failure.d_rate, reserve_before_failure.d_rate);
    assert_eq!(reserve_after_failure.b_rate, reserve_before_failure.b_rate);
    assert_eq!(
        reserve_after_failure.last_time,
        reserve_before_failure.last_time
    );
    assert!(pool_fixture.pool.try_get_bad_debt_auction().is_err());

    set_mock_backstop_valuation_version(&fixture.env, &valuation, 1);
    let accrued_reserve = pool_fixture.pool.get_reserve(&stable);
    let pool_stable_before = fixture.tokens[TokenIndex::STABLE].balance(&pool_fixture.pool.address);
    let backstop_stable_before =
        fixture.tokens[TokenIndex::STABLE].balance(&fixture.backstop.address);
    let continuation = pool_fixture.pool.continue_bad_debt_resolution(&auction_id);
    assert!(!continuation.auction_created);
    assert_eq!(continuation.defaulted.get(stable.clone()), Some(debt));

    let default_underlying = debt
        .fixed_mul_ceil(accrued_reserve.data.d_rate, SCALAR_12)
        .unwrap();
    let b_rate_loss = default_underlying
        .fixed_div_ceil(accrued_reserve.data.b_supply, SCALAR_12)
        .unwrap();
    let expected_b_rate = (accrued_reserve.data.b_rate - b_rate_loss).max(0);
    let reserve_after = fixture.read_reserve_data(0, TokenIndex::STABLE);
    assert_eq!(reserve_after.d_supply, accrued_reserve.data.d_supply - debt);
    assert_eq!(reserve_after.b_supply, accrued_reserve.data.b_supply);
    assert_eq!(reserve_after.b_rate, expected_b_rate);
    assert_eq!(
        fixture.tokens[TokenIndex::STABLE].balance(&pool_fixture.pool.address),
        pool_stable_before
    );
    assert_eq!(
        fixture.tokens[TokenIndex::STABLE].balance(&fixture.backstop.address),
        backstop_stable_before
    );
    assert!(pool_fixture
        .pool
        .get_positions(&fixture.backstop.address)
        .liabilities
        .is_empty());
    assert_eq!(
        pool_fixture.pool.backstop_loss_state(),
        BackstopLossState {
            committed_loss_entries: 0,
            liability_entries: 0,
            unresolved_bad_debt_entries: 0,
        }
    );
    assert!(pool_fixture
        .pool
        .backstop_withdrawal_allowed(&fixture.backstop.address));

    let dust = 1_i128;
    let dust_positions = Positions {
        liabilities: map![&fixture.env, (stable_index, dust)],
        collateral: map![&fixture.env],
        supply: map![&fixture.env],
    };
    let dust_records = CanonicalBackstopLossRecords {
        committed_losses: map![&fixture.env],
        liabilities: map![&fixture.env, (stable.clone(), dust)],
        unresolved_bad_debt: map![&fixture.env],
    };
    fixture.env.as_contract(&pool_fixture.pool.address, || {
        let key = PoolDataKey::ResData(stable.clone());
        let mut reserve: ReserveData = fixture.env.storage().persistent().get(&key).unwrap();
        reserve.d_supply += dust;
        fixture.env.storage().persistent().set(&key, &reserve);
        fixture.env.storage().persistent().set(
            &PoolDataKey::Positions(fixture.backstop.address.clone()),
            &dust_positions,
        );
        fixture
            .env
            .storage()
            .instance()
            .set(&Symbol::new(&fixture.env, "LossRec"), &dust_records);
    });
    fixture.oracle.set_price_stable(&vec![
        &fixture.env,
        2000 * SCALAR_7,
        SCALAR_7,
        SCALAR_7 / 10,
        1,
    ]);

    let dust_continuation = pool_fixture
        .pool
        .continue_bad_debt_resolution(&BytesN::from_array(&fixture.env, &[14; 32]));
    assert!(!dust_continuation.auction_created);
    assert_eq!(dust_continuation.defaulted.get(stable), Some(dust));
    assert!(pool_fixture
        .pool
        .get_positions(&fixture.backstop.address)
        .liabilities
        .is_empty());
    assert!(pool_fixture
        .pool
        .backstop_withdrawal_allowed(&fixture.backstop.address));
}

#[test]
fn test_wasm_max_reserve_supplier_default_fits_mainnet_invocation_limits() {
    let fixture = create_fixture_with_data(true);
    let pool_fixture = &fixture.pools[0];
    let frodo = fixture.users[0].clone();
    let deposited_shares = 50_000 * SCALAR_7;

    fixture.backstop.distribute();
    fixture.backstop.queue_blnd_usdc_withdrawal(
        &frodo,
        &pool_fixture.pool.address,
        &deposited_shares,
    );
    fixture.jump(17 * 24 * 60 * 60 + 1);
    fixture.backstop.distribute();
    fixture.backstop.withdraw_blnd_usdc(
        &frodo,
        &pool_fixture.pool.address,
        &deposited_shares,
        &frodo,
    );

    let dusty_tier_assets = 100 * SCALAR_7;
    fixture
        .backstop
        .deposit_blnd_usdc(&frodo, &pool_fixture.pool.address, &dusty_tier_assets);
    let dust_provider = Address::generate(&fixture.env);
    let blnd_xlm_address = fixture.backstop.tier_token(&BackstopContractTier::BlndXlm);
    let blnd_xlm = LPClient::new(&fixture.env, &blnd_xlm_address);
    fixture.tokens[TokenIndex::BLND].mint(&dust_provider, &(1_000 * SCALAR_7));
    fixture.tokens[TokenIndex::XLM].mint(&dust_provider, &(25 * SCALAR_7));
    blnd_xlm.join_pool(
        &dusty_tier_assets,
        &vec![&fixture.env, 1_000 * SCALAR_7, 25 * SCALAR_7],
        &dust_provider,
    );
    fixture.backstop.deposit_blnd_xlm(
        &dust_provider,
        &pool_fixture.pool.address,
        &dusty_tier_assets,
    );
    fixture.tokens[TokenIndex::USDC].mint(&dust_provider, &dusty_tier_assets);
    fixture.backstop.deposit_usdc(
        &dust_provider,
        &pool_fixture.pool.address,
        &dusty_tier_assets,
    );

    let mut reserve_assets = pool_fixture.pool.get_reserve_list();
    while reserve_assets.len() < 30 {
        reserve_assets.push_back(Address::generate(&fixture.env));
    }
    let mut oracle_assets = soroban_sdk::Vec::new(&fixture.env);
    let mut oracle_prices = soroban_sdk::Vec::new(&fixture.env);
    for asset in reserve_assets.iter() {
        oracle_assets.push_back(Asset::Stellar(asset));
        oracle_prices.push_back(SCALAR_7);
    }
    fixture.oracle.set_data(
        &fixture.bombadil,
        &Asset::Other(Symbol::new(&fixture.env, "USD")),
        &oracle_assets,
        &7,
        &300,
    );
    fixture.oracle.set_price_stable(&oracle_prices);

    let mut positions = Positions {
        liabilities: map![&fixture.env],
        collateral: map![&fixture.env],
        supply: map![&fixture.env],
    };
    let mut records = CanonicalBackstopLossRecords {
        committed_losses: map![&fixture.env],
        liabilities: map![&fixture.env],
        unresolved_bad_debt: map![&fixture.env],
    };
    let mut pool_config = pool_fixture.pool.get_config();
    pool_config.max_positions = 60;
    let template_config = fixture.read_reserve_config(0, TokenIndex::XLM);
    let now = fixture.env.ledger().timestamp();

    fixture.env.as_contract(&pool_fixture.pool.address, || {
        fixture
            .env
            .storage()
            .instance()
            .set(&Symbol::new(&fixture.env, "Config"), &pool_config);
        fixture
            .env
            .storage()
            .persistent()
            .set(&Symbol::new(&fixture.env, "ResList"), &reserve_assets);

        for (index, asset) in reserve_assets.iter().enumerate() {
            let index = index as u32;
            let debt = 1_i128;
            positions.liabilities.set(index, debt);
            records.liabilities.set(asset.clone(), debt);

            if index < 3 {
                let key = PoolDataKey::ResData(asset);
                let mut data: ReserveData = fixture.env.storage().persistent().get(&key).unwrap();
                data.d_supply += debt;
                data.last_time = now;
                fixture.env.storage().persistent().set(&key, &data);
            } else {
                let mut config = template_config.clone();
                config.index = index;
                config.decimals = 7;
                let data = ReserveData {
                    d_rate: SCALAR_12,
                    b_rate: SCALAR_12,
                    ir_mod: SCALAR_12,
                    b_supply: 100 * SCALAR_7,
                    d_supply: debt,
                    backstop_credit: 0,
                    last_time: now,
                };
                fixture
                    .env
                    .storage()
                    .persistent()
                    .set(&PoolDataKey::ResConfig(asset.clone()), &config);
                fixture
                    .env
                    .storage()
                    .persistent()
                    .set(&PoolDataKey::ResData(asset), &data);
            }
        }
        fixture.env.storage().persistent().set(
            &PoolDataKey::Positions(fixture.backstop.address.clone()),
            &positions,
        );
        fixture
            .env
            .storage()
            .instance()
            .set(&Symbol::new(&fixture.env, "LossRec"), &records);
    });

    assert_eq!(
        pool_fixture.pool.backstop_loss_state(),
        BackstopLossState {
            committed_loss_entries: 0,
            liability_entries: 30,
            unresolved_bad_debt_entries: 0,
        }
    );
    fixture.env.cost_estimate().budget().reset_unlimited();
    let continuation = pool_fixture
        .pool
        .continue_bad_debt_resolution(&BytesN::from_array(&fixture.env, &[13; 32]));
    let resources = fixture.env.cost_estimate().resources();

    assert!(!continuation.auction_created);
    assert_eq!(continuation.defaulted.len(), 30);
    assert!(resources.instructions <= 600_000_000);
    assert!(resources.mem_bytes <= 41_943_040);
    assert!(resources.disk_read_entries <= 100);
    assert!(resources.write_entries <= 50);
    // Preserve ten read-footprint entries of headroom for production oracle
    // implementations beneath the Protocol-27 100-entry ceiling.
    assert!(
        resources.disk_read_entries + resources.memory_read_entries <= 90,
        "resources: {resources:?}"
    );
    assert!(resources.disk_read_bytes <= 200_000);
    assert!(resources.write_bytes <= 132_096);
    assert!(resources.contract_events_size_bytes <= 16_384);
    assert!(pool_fixture
        .pool
        .get_positions(&fixture.backstop.address)
        .liabilities
        .is_empty());
    assert!(pool_fixture
        .pool
        .backstop_withdrawal_allowed(&fixture.backstop.address));
}

/// Smoke test for managing positions, tracking emissions, and accruing interest
#[test]
fn test_wasm_happy_path() {
    let fixture = create_fixture_with_data(false);
    let frodo = fixture.users.get(0).unwrap();
    let pool_fixture = &fixture.pools[0];
    let stable_pool_index = pool_fixture.reserves[&TokenIndex::STABLE];
    let xlm_pool_index = pool_fixture.reserves[&TokenIndex::XLM];

    assert_eq!(
        pool_fixture.pool.backstop_loss_state(),
        BackstopLossState {
            committed_loss_entries: 0,
            liability_entries: 0,
            unresolved_bad_debt_entries: 0,
        }
    );
    assert!(pool_fixture
        .pool
        .backstop_withdrawal_allowed(&fixture.backstop.address));

    // Create two new users
    let sam = Address::generate(&fixture.env); // sam will be supplying XLM and borrowing STABLE
    let merry = Address::generate(&fixture.env); // merry will be supplying STABLE and borrowing XLM

    // Mint users tokens
    let stable = &fixture.tokens[TokenIndex::STABLE];
    let xlm = &fixture.tokens[TokenIndex::XLM];
    let mut sam_stable_balance = 60_000 * 10i128.pow(6);
    let mut sam_xlm_balance = 2_500_000 * SCALAR_7;
    let mut merry_stable_balance = 250_000 * 10i128.pow(6);
    let mut merry_xlm_balance = 600_000 * SCALAR_7;
    stable.mint(&sam, &sam_stable_balance);
    stable.mint(&merry, &merry_stable_balance);
    xlm.mint(&sam, &sam_xlm_balance);
    xlm.mint(&merry, &merry_xlm_balance);

    let mut pool_stable_balance = stable.balance(&pool_fixture.pool.address);
    let mut pool_xlm_balance = xlm.balance(&pool_fixture.pool.address);

    let mut sam_xlm_btoken_balance = 0;
    let mut sam_stable_dtoken_balance = 0;
    let mut merry_stable_btoken_balance = 0;
    let mut merry_xlm_dtoken_balance = 0;

    // Merry supply STABLE
    let amount = 190_000 * 10i128.pow(6);
    let result = pool_fixture.pool.submit(
        &merry,
        &merry,
        &merry,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::SupplyCollateral as u32,
                address: stable.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::STABLE);
    pool_stable_balance += amount;
    merry_stable_balance -= amount;
    assert_eq!(stable.balance(&merry), merry_stable_balance);
    assert_eq!(
        stable.balance(&pool_fixture.pool.address),
        pool_stable_balance
    );
    merry_stable_btoken_balance += amount
        .fixed_div_floor(reserve_data.b_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.collateral.get_unchecked(stable_pool_index),
        merry_stable_btoken_balance,
        10,
    );

    // Sam supply XLM
    let amount = 1_900_000 * SCALAR_7;
    let result = pool_fixture.pool.submit(
        &sam,
        &sam,
        &sam,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::SupplyCollateral as u32,
                address: xlm.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::XLM);
    pool_xlm_balance += amount;
    sam_xlm_balance -= amount;
    assert_eq!(xlm.balance(&sam), sam_xlm_balance);
    assert_eq!(xlm.balance(&pool_fixture.pool.address), pool_xlm_balance);
    sam_xlm_btoken_balance += amount
        .fixed_div_floor(reserve_data.b_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.collateral.get_unchecked(xlm_pool_index),
        sam_xlm_btoken_balance,
        10,
    );

    // Sam borrow STABLE
    let amount = 112_000 * 10i128.pow(6); // Sam max borrow is .75*.95*.1*1_900_000 = 135_375 STABLE
    let result = pool_fixture.pool.submit(
        &sam,
        &sam,
        &sam,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::Borrow as u32,
                address: stable.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::STABLE);
    pool_stable_balance -= amount;
    sam_stable_balance += amount;
    assert_eq!(stable.balance(&sam), sam_stable_balance);
    assert_eq!(
        stable.balance(&pool_fixture.pool.address),
        pool_stable_balance
    );
    sam_stable_dtoken_balance += amount
        .fixed_div_floor(reserve_data.d_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.liabilities.get_unchecked(stable_pool_index),
        sam_stable_dtoken_balance,
        10,
    );

    // Merry borrow XLM
    let amount = 1_135_000 * SCALAR_7; // Merry max borrow is .75*.9*190_000/.1 = 1_282_5000 XLM
    let result = pool_fixture.pool.submit(
        &merry,
        &merry,
        &merry,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::Borrow as u32,
                address: xlm.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::XLM);
    pool_xlm_balance -= amount;
    merry_xlm_balance += amount;
    assert_eq!(xlm.balance(&merry), merry_xlm_balance);
    assert_eq!(xlm.balance(&pool_fixture.pool.address), pool_xlm_balance);
    merry_xlm_dtoken_balance += amount
        .fixed_div_floor(reserve_data.d_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.liabilities.get_unchecked(xlm_pool_index),
        merry_xlm_dtoken_balance,
        10,
    );

    // Utilization is now:
    // * 120_000 / 200_000 = .625 for STABLE
    // * 1_200_000 / 2_000_000 = .625 for XLM
    // This equates to the following rough annual interest rates
    //  * 19.9% for XLM borrowing
    //  * 11.1% for XLM lending
    //  * rate will be dragged up due to rate modifier
    //  * 4.7% for STABLE borrowing
    //  * 2.6% for STABLE lending
    //  * rate will be dragged down due to rate modifier

    // claim frodo's setup emissions (1h1m passes during setup)
    // - Frodo should receive 60 * 61 * .3 = 1098 BLND from the pool claim
    // - The backstop tranche is allocated immediately for the full first
    //   emission cycle, unlike the pool tranche's seven-day stream.
    let mut backstop_blnd_balance =
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address);
    let claim_amount = pool_fixture
        .pool
        .claim(&frodo, &vec![&fixture.env, 0, 3], &frodo);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 1098_0000000);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );
    let backstop_claim =
        fixture
            .backstop
            .claim_ongoing_blnd(&frodo, &pool_fixture.pool.address, &frodo);
    assert_eq!(backstop_claim, 423_360_0000000);
    backstop_blnd_balance -= backstop_claim;
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );

    // Let three days pass
    pool_fixture.pool.gulp(&stable.address);
    fixture.jump(60 * 60 * 24 * 3);

    // Claim 3 day emissions

    // Claim frodo's three day pool emissions
    let frodo_balance = fixture.tokens[TokenIndex::BLND].balance(&frodo);
    let claim_amount = pool_fixture
        .pool
        .claim(&frodo, &vec![&fixture.env, 0, 3], &frodo);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 4665_6412730);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&frodo),
        frodo_balance + claim_amount
    );

    // Claim sam's three day pool emissions
    let sam_balance = fixture.tokens[TokenIndex::BLND].balance(&sam);
    let claim_amount = pool_fixture
        .pool
        .claim(&sam, &vec![&fixture.env, 0, 3], &sam);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 730943587268);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&sam),
        sam_balance + claim_amount
    );

    // Sam repays some of his STABLE loan
    let amount = 55_000 * 10i128.pow(6);
    let result = pool_fixture.pool.submit(
        &sam,
        &sam,
        &sam,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::Repay as u32,
                address: stable.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::STABLE);
    pool_stable_balance += amount;
    sam_stable_balance -= amount;
    assert_eq!(stable.balance(&sam), sam_stable_balance);
    assert_eq!(
        stable.balance(&pool_fixture.pool.address),
        pool_stable_balance
    );
    sam_stable_dtoken_balance -= amount
        .fixed_div_floor(reserve_data.d_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.liabilities.get_unchecked(stable_pool_index),
        sam_stable_dtoken_balance,
        10,
    );

    // Merry repays some of his XLM loan
    let amount = 575_000 * SCALAR_7;
    let result = pool_fixture.pool.submit(
        &merry,
        &merry,
        &merry,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::Repay as u32,
                address: xlm.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::XLM);
    pool_xlm_balance += amount;
    merry_xlm_balance -= amount;
    assert_eq!(xlm.balance(&merry), merry_xlm_balance);
    assert_eq!(xlm.balance(&pool_fixture.pool.address), pool_xlm_balance);
    merry_xlm_dtoken_balance -= amount
        .fixed_div_floor(reserve_data.d_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.liabilities.get_unchecked(xlm_pool_index),
        merry_xlm_dtoken_balance,
        10,
    );

    // Sam withdraws some of his XLM
    let amount = 1_000_000 * SCALAR_7;
    let result = pool_fixture.pool.submit(
        &sam,
        &sam,
        &sam,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::WithdrawCollateral as u32,
                address: xlm.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::XLM);
    pool_xlm_balance -= amount;
    sam_xlm_balance += amount;
    assert_eq!(xlm.balance(&sam), sam_xlm_balance);
    assert_eq!(xlm.balance(&pool_fixture.pool.address), pool_xlm_balance);
    sam_xlm_btoken_balance -= amount
        .fixed_div_floor(reserve_data.b_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.collateral.get_unchecked(xlm_pool_index),
        sam_xlm_btoken_balance,
        10,
    );

    // Merry withdraws some of his STABLE
    let amount = 100_000 * 10i128.pow(6);
    let result = pool_fixture.pool.submit(
        &merry,
        &merry,
        &merry,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::WithdrawCollateral as u32,
                address: stable.address.clone(),
                amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::STABLE);
    pool_stable_balance -= amount;
    merry_stable_balance += amount;
    assert_eq!(stable.balance(&merry), merry_stable_balance);
    assert_eq!(
        stable.balance(&pool_fixture.pool.address),
        pool_stable_balance
    );
    merry_stable_btoken_balance -= amount
        .fixed_div_floor(reserve_data.b_rate, SCALAR_12)
        .unwrap();
    assert_approx_eq_abs(
        result.collateral.get_unchecked(stable_pool_index),
        merry_stable_btoken_balance,
        10,
    );

    // Let rest of emission period pass
    fixture.jump(341940);

    // Distribute emissions
    fixture.backstop.distribute();
    pool_fixture.pool.gulp_emissions();

    // Frodo claim emissions
    let mut backstop_blnd_balance =
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address);
    let frodo_balance = fixture.tokens[TokenIndex::BLND].balance(&frodo);
    let claim_amount = pool_fixture
        .pool
        .claim(&frodo, &vec![&fixture.env, 0, 3], &frodo);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 11673_1666150);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&frodo),
        frodo_balance + claim_amount
    );

    let backstop_claim =
        fixture
            .backstop
            .claim_ongoing_blnd(&frodo, &pool_fixture.pool.address, &frodo);
    assert_eq!(backstop_claim, 4233600000000);
    backstop_blnd_balance -= backstop_claim;
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );

    // Sam claim emissions
    let sam_balance = fixture.tokens[TokenIndex::BLND].balance(&sam);
    let claim_amount = pool_fixture
        .pool
        .claim(&sam, &vec![&fixture.env, 0, 3], &sam);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 90908_8333850);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&sam),
        sam_balance + claim_amount
    );

    // Let 51 weeks go by and call update to validate emissions won't get missed
    pool_fixture.pool.gulp(&stable.address);

    fixture.jump(60 * 60 * 24 * 7 * 51);
    fixture.backstop.distribute();
    pool_fixture.pool.gulp_emissions();
    // Allow another week go by to distribute missed emissions
    pool_fixture.pool.gulp(&stable.address);

    fixture.jump(60 * 60 * 24 * 7);
    fixture.backstop.distribute();
    pool_fixture.pool.gulp_emissions();

    // Frodo claims a year worth of backstop emissions
    let mut backstop_blnd_balance =
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address);
    let backstop_claim =
        fixture
            .backstop
            .claim_ongoing_blnd(&frodo, &pool_fixture.pool.address, &frodo);
    assert_eq!(backstop_claim, 22_014_720_0000000);
    backstop_blnd_balance -= backstop_claim;
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );

    // Frodo claims a year worth of pool emissions
    let claim_amount = pool_fixture
        .pool
        .claim(&frodo, &vec![&fixture.env, 0, 3], &frodo);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 1073628_1826494);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );

    // Sam claims a year worth of pool emissions
    let claim_amount = pool_fixture
        .pool
        .claim(&sam, &vec![&fixture.env, 0, 3], &sam);
    backstop_blnd_balance -= claim_amount;
    assert_eq!(claim_amount, 8361251_8173506);
    assert_eq!(
        fixture.tokens[TokenIndex::BLND].balance(&fixture.backstop.address),
        backstop_blnd_balance
    );

    // Sam repays his STABLE loan
    let amount = sam_stable_dtoken_balance
        .fixed_mul_ceil(1_100_000_000_000, SCALAR_12)
        .unwrap();
    let result = pool_fixture.pool.submit(
        &sam,
        &sam,
        &sam,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::Repay as u32,
                address: stable.address.clone(),
                amount: amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::STABLE);
    let est_amount = sam_stable_dtoken_balance
        .fixed_mul_ceil(reserve_data.d_rate, SCALAR_12)
        .unwrap();
    pool_stable_balance += est_amount;
    sam_stable_balance -= est_amount;
    assert_approx_eq_abs(stable.balance(&sam), sam_stable_balance, 100);
    assert_approx_eq_abs(
        stable.balance(&pool_fixture.pool.address),
        pool_stable_balance,
        100,
    );
    assert_eq!(result.liabilities.get(stable_pool_index), None);
    assert_eq!(result.liabilities.len(), 0);

    // Merry repays his XLM loan
    let amount = merry_xlm_dtoken_balance
        .fixed_mul_ceil(1_250_000_000_000, SCALAR_12)
        .unwrap();
    let result = pool_fixture.pool.submit(
        &merry,
        &merry,
        &merry,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::Repay as u32,
                address: xlm.address.clone(),
                amount: amount,
            },
        ],
    );
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::XLM);
    let est_amount = merry_xlm_dtoken_balance
        .fixed_mul_ceil(reserve_data.d_rate, SCALAR_12)
        .unwrap();
    pool_xlm_balance += est_amount;
    merry_xlm_balance -= est_amount;
    assert_approx_eq_abs(xlm.balance(&merry), merry_xlm_balance, 100);
    assert_approx_eq_abs(
        xlm.balance(&pool_fixture.pool.address),
        pool_xlm_balance,
        100,
    );
    assert_eq!(result.liabilities.get(xlm_pool_index), None);
    assert_eq!(result.liabilities.len(), 0);

    // Sam withdraws all of his XLM
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::XLM);
    let amount = sam_xlm_btoken_balance
        .fixed_mul_ceil(reserve_data.b_rate, SCALAR_12)
        .unwrap();
    let result = pool_fixture.pool.submit(
        &sam,
        &sam,
        &sam,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::WithdrawCollateral as u32,
                address: xlm.address.clone(),
                amount: amount,
            },
        ],
    );
    pool_xlm_balance -= amount;
    sam_xlm_balance += amount;
    assert_approx_eq_abs(xlm.balance(&sam), sam_xlm_balance, 10);
    assert_approx_eq_abs(
        xlm.balance(&pool_fixture.pool.address),
        pool_xlm_balance,
        10,
    );

    assert_eq!(result.collateral.get(xlm_pool_index), None);

    let expected_gulp_amount = 100 * SCALAR_7;
    stable.mint(&pool_fixture.pool.address, &expected_gulp_amount);
    let gulp_amount = pool_fixture.pool.gulp(&stable.address);
    assert_eq!(gulp_amount, expected_gulp_amount + 2); // 2 stroops from rounding loss
    pool_stable_balance += expected_gulp_amount; // rounding loss does not effect the b_rate

    // Merry withdraws all of his STABLE
    let reserve_data = fixture.read_reserve_data(0, TokenIndex::STABLE);
    let amount = merry_stable_btoken_balance
        .fixed_mul_ceil(reserve_data.b_rate, SCALAR_12)
        .unwrap();
    let result = pool_fixture.pool.submit(
        &merry,
        &merry,
        &merry,
        &vec![
            &fixture.env,
            Request {
                request_type: RequestType::WithdrawCollateral as u32,
                address: stable.address.clone(),
                amount: amount,
            },
        ],
    );
    pool_stable_balance -= amount;
    merry_stable_balance += amount;
    assert_approx_eq_abs(stable.balance(&merry), merry_stable_balance, 10);

    assert_approx_eq_abs(
        stable.balance(&pool_fixture.pool.address),
        pool_stable_balance,
        10,
    );

    assert_eq!(result.collateral.get(stable_pool_index), None);

    // Frodo queues for withdrawal a portion of his backstop deposit
    // Backstop shares are still 1 to 1 with BSTOP tokens - no donation via auction or other means has occurred
    let mut frodo_bstop_token_balance = fixture.lp.balance(&frodo);
    let mut backstop_bstop_token_balance = fixture.lp.balance(&fixture.backstop.address);
    let amount = 500 * SCALAR_7;
    let result =
        fixture
            .backstop
            .queue_blnd_usdc_withdrawal(&frodo, &pool_fixture.pool.address, &amount);
    assert_eq!(result.amount, amount);
    assert_eq!(
        result.exp,
        fixture.env.ledger().timestamp() + 60 * 60 * 24 * 17
    );
    assert_eq!(fixture.lp.balance(&frodo), frodo_bstop_token_balance);
    assert_eq!(
        fixture.lp.balance(&fixture.backstop.address),
        backstop_bstop_token_balance
    );

    // Time passes and Frodo withdraws his queued for withdrawal backstop deposit
    pool_fixture.pool.gulp(&stable.address);

    fixture.jump(60 * 60 * 24 * 17 + 1);
    fixture.backstop.distribute();
    let result =
        fixture
            .backstop
            .withdraw_blnd_usdc(&frodo, &pool_fixture.pool.address, &amount, &frodo);
    frodo_bstop_token_balance += result;
    backstop_bstop_token_balance -= result;
    assert_eq!(result, amount);
    assert_eq!(fixture.lp.balance(&frodo), frodo_bstop_token_balance);
    assert_eq!(
        fixture.lp.balance(&fixture.backstop.address),
        backstop_bstop_token_balance
    );
}
