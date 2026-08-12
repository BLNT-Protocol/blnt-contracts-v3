use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopPoolData},
    errors::PoolError,
    pool::{Pool, User},
    storage,
};
use cast::i128;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{contracttype, map, panic_with_error, Address, Env, Map, Vec};

use super::{
    get_tier_auction, has_tier_auction,
    math::{
        auction_modifiers, proportional_ceil, proportional_floor, scale_bid_amount,
        scale_lot_amount,
    },
    remove_tier_auction, set_tier_auction, to_backstop_tier, AuctionData, AuctionType,
    BackstopTier, TierAuctionData,
};

const BAD_DEBT_LOT_PREMIUM_NUMERATOR: i128 = 6;
const BAD_DEBT_LOT_PREMIUM_DENOMINATOR: i128 = 5;
/// Preserve footprint headroom for stateful per-reserve oracle reads while a
/// creation call also validates the complete bounded reserve set.
const MAX_BAD_DEBT_BID_ASSETS: u32 = 4;

/// Exact amounts processed by one bad-debt auction fill.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub(crate) struct BadDebtAuctionFill {
    /// Base lot removed from the pool's selection before the time modifier.
    pub base_lot_amount: i128,
    /// Time-scaled dToken shares assumed by the filler.
    pub bid: Map<Address, i128>,
    pub block: u32,
    pub complete: bool,
    /// Time-scaled tier-token amount transferred to the filler.
    pub lot_amount: i128,
    pub tier: BackstopTier,
}

/// Create the next canonical bad-debt auction.
pub fn create_bad_debt_auction(e: &Env) -> TierAuctionData {
    require_no_active_bad_debt_auction(e);
    let mut pool = Pool::load(e);
    let bid = canonical_bad_debt_bid(e, &pool);
    let (bid_amounts, debt_value_usdc) = build_bad_debt_bid(e, &mut pool, &bid);
    commit_prepared_bad_debt_auction(e, &bid_amounts, debt_value_usdc)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::InvalidLot))
}

/// Default residual backstop debt only after verifying that every tier has no
/// usable value.
pub fn default_backstop_bad_debt(e: &Env) {
    require_no_active_bad_debt_auction(e);
    let pool = Pool::load(e);
    canonical_bad_debt_bid(e, &pool);
    if quote_bad_debt_lot(e, 1).is_some() {
        panic_with_error!(e, PoolError::BadRequest);
    }
    default_all_backstop_liabilities(e, pool);
}

fn require_no_active_bad_debt_auction(e: &Env) {
    let backstop = storage::get_backstop(e);
    if has_tier_auction(e, AuctionType::BadDebtAuction)
        || storage::has_auction(e, &(AuctionType::BadDebtAuction as u32), &backstop)
    {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }
}

fn build_bad_debt_bid(e: &Env, pool: &mut Pool, bid: &Vec<Address>) -> (Map<Address, i128>, i128) {
    let backstop = storage::get_backstop(e);
    let oracle_scalar = 10i128.pow(pool.load_price_decimals(e));
    let backstop_positions = storage::get_user_positions(e, &backstop);
    let mut bid_amounts = Map::<Address, i128>::new(e);
    let mut debt_value = 0_i128;
    for bid_asset in bid {
        let reserve = pool.load_reserve(e, &bid_asset, false);
        let liability_balance = backstop_positions
            .liabilities
            .get(reserve.config.index)
            .unwrap_or(0);
        if liability_balance <= 0 {
            panic_with_error!(e, PoolError::InvalidBid);
        }
        let asset_to_base = pool.load_price(e, &reserve.asset);
        let asset_balance = reserve.to_asset_from_d_token(e, liability_balance);
        let asset_value = i128(asset_to_base).fixed_mul_floor(e, &asset_balance, &reserve.scalar);
        debt_value = debt_value
            .checked_add(asset_value)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        bid_amounts.set(reserve.asset, liability_balance);
    }

    if bid_amounts.is_empty() {
        panic_with_error!(e, PoolError::InvalidBid);
    }
    // A positive dToken liability can be worth less than one oracle base unit
    // after the inherited per-reserve floor. Quote that dust at the smallest
    // representable positive USDC amount so it cannot strand the deterministic
    // next creation call ahead of later liabilities or supplier settlement.
    let debt_value_usdc = if debt_value == 0 {
        1
    } else {
        debt_value.fixed_mul_ceil(e, &SCALAR_7, &oracle_scalar)
    };
    if debt_value_usdc <= 0 {
        panic_with_error!(e, PoolError::InvalidBid);
    }
    (bid_amounts, debt_value_usdc)
}

fn commit_prepared_bad_debt_auction(
    e: &Env,
    bid_amounts: &Map<Address, i128>,
    debt_value_usdc: i128,
) -> Option<TierAuctionData> {
    let (tier, lot_amount) = quote_bad_debt_lot(e, debt_value_usdc)?;
    if lot_amount <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let block = e
        .ledger()
        .sequence()
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let backstop = storage::get_backstop(e);
    let lot_token = BackstopClient::new(e, &backstop).backstop_token(&to_backstop_tier(tier));
    let auction = TierAuctionData {
        auction: AuctionData {
            bid: bid_amounts.clone(),
            lot: map![e, (lot_token, lot_amount)],
            block,
        },
        tier,
    };
    set_tier_auction(e, AuctionType::BadDebtAuction, &auction);
    Some(auction)
}

fn quote_bad_debt_lot(e: &Env, debt_value_usdc: i128) -> Option<(BackstopTier, i128)> {
    let backstop = storage::get_backstop(e);
    let pool_address = e.current_contract_address();
    let pool_data = BackstopClient::new(e, &backstop).pool_data(&pool_address);
    select_bad_debt_lot(e, &pool_data, debt_value_usdc)
}

fn canonical_bad_debt_bid(e: &Env, pool: &Pool) -> Vec<Address> {
    let backstop = storage::get_backstop(e);
    let backstop_positions = storage::get_user_positions(e, &backstop);
    let position_bound = pool
        .config
        .max_positions
        .checked_sub(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let max_bid = core::cmp::min(position_bound, MAX_BAD_DEBT_BID_ASSETS);
    let mut bid = Vec::new(e);
    let mut liability_count = 0_u32;

    for asset in storage::get_res_list(e) {
        let reserve_config = storage::get_res_config(e, &asset);
        let amount = backstop_positions
            .liabilities
            .get(reserve_config.index)
            .unwrap_or(0);
        if amount < 0 {
            panic_with_error!(e, PoolError::InvalidBid);
        }
        if amount > 0 {
            liability_count = liability_count
                .checked_add(1)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            if bid.len() < max_bid {
                bid.push_back(asset);
            }
        }
    }
    if liability_count == 0 || backstop_positions.liabilities.len() != liability_count {
        panic_with_error!(e, PoolError::InvalidBid);
    }
    bid
}

fn default_all_backstop_liabilities(e: &Env, mut pool: Pool) {
    let backstop = storage::get_backstop(e);
    let mut backstop_state = User::load(e, &backstop);
    let mut defaulted = false;

    for asset in storage::get_res_list(e) {
        let reserve_config = storage::get_res_config(e, &asset);
        let amount = backstop_state.get_liabilities(reserve_config.index);
        if amount == 0 {
            continue;
        }
        let mut reserve = pool.load_reserve(e, &asset, true);
        backstop_state.default_liabilities(e, &mut reserve, amount);
        pool.cache_reserve(reserve);
        defaulted = true;
        crate::events::PoolEvents::defaulted_debt(e, asset, amount);
    }

    if backstop_state.has_liabilities() || !defaulted {
        panic_with_error!(e, PoolError::InvalidBid);
    }
    backstop_state.store(e);
    pool.store_cached_reserves(e);
}

pub fn get_prepared_bad_debt_auction(e: &Env) -> TierAuctionData {
    get_tier_auction(e, AuctionType::BadDebtAuction)
}

/// Atomically transfer the scaled debt bid and settle the realized tier loss.
pub fn fill_prepared_bad_debt_auction(
    e: &Env,
    pool: &mut Pool,
    auction_user: &Address,
    filler_state: &mut User,
    percent: u32,
) -> AuctionData {
    let pool_address = e.current_contract_address();
    let backstop = storage::get_backstop(e);
    if auction_user != &backstop
        || filler_state.address == pool_address
        || filler_state.address == backstop
        || percent == 0
        || percent > 100
    {
        panic_with_error!(e, PoolError::InvalidLiquidation);
    }
    if storage::has_auction(
        e,
        &(AuctionType::UserLiquidation as u32),
        &filler_state.address,
    ) {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let auction = get_prepared_bad_debt_auction(e);
    let (fill, remaining_bid, remaining_lot_amount) =
        scale_prepared_bad_debt_auction(e, &auction, percent);
    let mut backstop_state = User::load(e, &backstop);

    backstop_state.rm_positions(e, pool, map![e], fill.bid.clone());
    filler_state.add_positions(e, pool, map![e], fill.bid.clone());

    BackstopClient::new(e, &backstop).draw(
        &to_backstop_tier(fill.tier),
        &pool_address,
        &fill.lot_amount,
        &filler_state.address,
    );
    if !fill.complete {
        set_prepared_bad_debt_auction(
            e,
            &TierAuctionData {
                auction: AuctionData {
                    bid: remaining_bid,
                    lot: map![
                        e,
                        (
                            auction.auction.lot.keys().get(0).unwrap(),
                            remaining_lot_amount
                        )
                    ],
                    block: auction.auction.block,
                },
                tier: auction.tier,
            },
        );
    } else {
        remove_tier_auction(e, AuctionType::BadDebtAuction);
    }

    backstop_state.store(e);
    let lot_token = BackstopClient::new(e, &backstop).backstop_token(&to_backstop_tier(fill.tier));
    let lot = if fill.lot_amount > 0 {
        map![e, (lot_token, fill.lot_amount)]
    } else {
        map![e]
    };
    AuctionData {
        bid: fill.bid,
        lot,
        block: fill.block,
    }
}

fn set_prepared_bad_debt_auction(e: &Env, auction: &TierAuctionData) {
    set_tier_auction(e, AuctionType::BadDebtAuction, auction);
}

fn scale_prepared_bad_debt_auction(
    e: &Env,
    auction: &TierAuctionData,
    percent: u32,
) -> (BadDebtAuctionFill, Map<Address, i128>, i128) {
    if percent == 0 || percent > 100 {
        panic_with_error!(e, PoolError::InvalidLiquidation);
    }
    let elapsed = e
        .ledger()
        .sequence()
        .checked_sub(auction.auction.block)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest));
    let (bid_modifier, lot_modifier) = auction_modifiers(e, elapsed);
    let percent_scaled = i128::from(percent)
        .checked_mul(100_000)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let mut filled_bid = Map::new(e);
    let mut remaining_bid = Map::new(e);
    for (asset, amount) in auction.auction.bid.iter() {
        let (_, scaled, remainder) = scale_bid_amount(e, amount, percent_scaled, bid_modifier);
        if remainder > 0 {
            remaining_bid.set(asset.clone(), remainder);
        }
        if scaled > 0 {
            filled_bid.set(asset, scaled);
        }
    }
    if auction.auction.lot.len() != 1 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let lot_amount = auction.auction.lot.values().get(0).unwrap();
    let (base_lot_amount, lot_amount, remaining_lot_amount) =
        scale_lot_amount(e, lot_amount, percent_scaled, lot_modifier);
    let complete = remaining_bid.is_empty() && remaining_lot_amount == 0;
    (
        BadDebtAuctionFill {
            base_lot_amount,
            bid: filled_bid,
            block: auction.auction.block,
            complete,
            lot_amount,
            tier: auction.tier,
        },
        remaining_bid,
        remaining_lot_amount,
    )
}

fn select_bad_debt_lot(
    e: &Env,
    pool_data: &BackstopPoolData,
    debt_value: i128,
) -> Option<(BackstopTier, i128)> {
    if debt_value <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let target_value = proportional_ceil(
        e,
        debt_value,
        BAD_DEBT_LOT_PREMIUM_NUMERATOR,
        BAD_DEBT_LOT_PREMIUM_DENOMINATOR,
    );
    for tier in [
        BackstopTier::BlndXlm,
        BackstopTier::BlndUsdc,
        BackstopTier::Usdc,
    ] {
        let (tokens, value) = match tier {
            BackstopTier::BlndUsdc => (pool_data.blnd_usdc.tokens, pool_data.blnd_usdc.value),
            BackstopTier::BlndXlm => (pool_data.blnd_xlm.tokens, pool_data.blnd_xlm.value),
            BackstopTier::Usdc => (pool_data.usdc.tokens, pool_data.usdc.value),
        };
        if tokens < 0 || value < 0 || (tokens == 0 && value > 0) {
            panic_with_error!(e, PoolError::InvalidLot);
        }
        if tokens == 0 || value == 0 {
            continue;
        }
        let lot_amount = allocate_bad_debt_tier(e, tokens, value, target_value);
        return Some((tier, lot_amount));
    }
    None
}

fn allocate_bad_debt_tier(
    e: &Env,
    available_assets: i128,
    available_value: i128,
    target_value: i128,
) -> i128 {
    if available_assets <= 0 || available_value <= 0 || target_value <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    if available_value <= target_value {
        return available_assets;
    }

    let lot_amount = proportional_ceil(e, target_value, available_assets, available_value);
    if lot_amount <= 0 || lot_amount > available_assets {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let committed_value = proportional_floor(e, available_value, lot_amount, available_assets);
    if committed_value < target_value {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    lot_amount
}

#[cfg(test)]
mod tests {

    use crate::{
        dependencies::BackstopPoolTierData,
        pool::{Positions, Request, RequestType},
        storage::PoolConfig,
        testutils::{self, create_pool},
    };

    use super::*;
    use sep_40_oracle::testutils::Asset;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        vec, Error, Symbol,
    };

    fn new_bad_debt(e: &Env, pool: &crate::PoolClient, backstop: &Address) -> AuctionData {
        pool.new_auction(&1, backstop, &vec![e], &vec![e], &100)
    }

    fn get_bad_debt(pool: &crate::PoolClient, backstop: &Address) -> AuctionData {
        pool.get_auction(&1, backstop)
    }

    fn del_bad_debt(pool: &crate::PoolClient, backstop: &Address) {
        pool.del_auction(&1, backstop);
    }

    fn fill_bad_debt(
        e: &Env,
        pool: &crate::PoolClient,
        backstop: &Address,
        filler: &Address,
        percent: i128,
    ) {
        pool.submit(
            filler,
            filler,
            filler,
            &vec![
                e,
                Request {
                    request_type: RequestType::FillBadDebtAuction as u32,
                    address: backstop.clone(),
                    amount: percent,
                },
            ],
        );
    }

    fn try_fill_bad_debt(
        e: &Env,
        pool: &crate::PoolClient,
        backstop: &Address,
        filler: &Address,
        percent: i128,
    ) -> bool {
        pool.try_submit(
            filler,
            filler,
            filler,
            &vec![
                e,
                Request {
                    request_type: RequestType::FillBadDebtAuction as u32,
                    address: backstop.clone(),
                    amount: percent,
                },
            ],
        )
        .is_ok()
    }

    fn test_tier_data(tokens: i128, value: i128) -> BackstopPoolTierData {
        BackstopPoolTierData {
            tokens,
            shares: tokens,
            value,
        }
    }

    fn test_pool_data(
        blnd_xlm: BackstopPoolTierData,
        blnd_usdc: BackstopPoolTierData,
        usdc: BackstopPoolTierData,
    ) -> BackstopPoolData {
        BackstopPoolData {
            active_value: blnd_xlm
                .value
                .checked_add(blnd_usdc.value)
                .and_then(|total| total.checked_add(usdc.value))
                .unwrap(),
            blnd_usdc,
            blnd_xlm,
            q4w_pct: 0,
            usdc,
        }
    }

    #[test]
    fn bad_debt_selection_enforces_the_strict_tier_waterfall() {
        let e = Env::default();
        let pool_data = test_pool_data(
            test_tier_data(200 * SCALAR_7, 200 * SCALAR_7),
            test_tier_data(300 * SCALAR_7, 300 * SCALAR_7),
            test_tier_data(400 * SCALAR_7, 400 * SCALAR_7),
        );

        let (tier, lot_amount) = select_bad_debt_lot(&e, &pool_data, 50 * SCALAR_7).unwrap();
        assert_eq!(tier, BackstopTier::BlndXlm);
        assert_eq!(lot_amount, 60 * SCALAR_7);
    }

    #[test]
    fn bad_debt_selection_uses_positive_subminimum_tiers() {
        let e = Env::default();
        let pool_data = test_pool_data(
            test_tier_data(99 * SCALAR_7, 99 * SCALAR_7),
            test_tier_data(0, 0),
            test_tier_data(500 * SCALAR_7, 500 * SCALAR_7),
        );

        let (tier, lot_amount) = select_bad_debt_lot(&e, &pool_data, 50 * SCALAR_7).unwrap();
        assert_eq!(tier, BackstopTier::BlndXlm);
        assert_eq!(lot_amount, 60 * SCALAR_7);
    }

    #[test]
    fn bad_debt_selection_skips_zero_value_tiers() {
        let e = Env::default();
        let pool_data = test_pool_data(
            test_tier_data(99 * SCALAR_7, 0),
            test_tier_data(0, 0),
            test_tier_data(500 * SCALAR_7, 500 * SCALAR_7),
        );

        let (tier, _) = select_bad_debt_lot(&e, &pool_data, 50 * SCALAR_7).unwrap();
        assert_eq!(tier, BackstopTier::Usdc);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1222)")]
    fn bad_debt_selection_rejects_value_without_assets() {
        let e = Env::default();
        let pool_data = test_pool_data(
            test_tier_data(0, SCALAR_7),
            test_tier_data(0, 0),
            test_tier_data(0, 0),
        );

        select_bad_debt_lot(&e, &pool_data, 50 * SCALAR_7);
    }

    #[test]
    fn bad_debt_partial_tier_selection_rounds_assets_up() {
        let e = Env::default();
        let pool_data = test_pool_data(
            test_tier_data(101, 200 * SCALAR_7),
            test_tier_data(0, 0),
            test_tier_data(0, 0),
        );

        let (_, lot_amount) = select_bad_debt_lot(&e, &pool_data, 100 * SCALAR_7).unwrap();
        assert_eq!(lot_amount, 61);
        assert!(proportional_floor(&e, 200 * SCALAR_7, lot_amount, 101) >= 120 * SCALAR_7);
    }

    #[test]
    fn prepared_bad_debt_auction_partially_fills_and_stale_delete_releases_lot() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();
        e.ledger().set(LedgerInfo {
            timestamp: 12_345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let admin = Address::generate(&e);
        let depositor = Address::generate(&e);
        let filler = Address::generate(&e);
        let unhealthy_filler = Address::generate(&e);
        let pool_address = create_pool(&e);
        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &admin);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &admin);
        let (lp_token, lp_token_client) = testutils::create_comet_lp_pool(&e, &admin, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

        blnd_client.mint(&depositor, &500_001_0000000);
        blnd_client.approve(&depositor, &lp_token, &i128::MAX, &99_999);
        usdc_client.mint(&depositor, &12_501_0000000);
        usdc_client.approve(&depositor, &lp_token, &i128::MAX, &99_999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &depositor,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &depositor,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);
        let (debt_asset, _) = testutils::create_token_contract(&e, &admin);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.index = 0;
        reserve_data.last_time = 12_345;
        testutils::create_reserve(
            &e,
            &pool_address,
            &debt_asset,
            &reserve_config,
            &reserve_data,
        );
        oracle_client.set_data(
            &admin,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![&e, Asset::Stellar(debt_asset.clone())],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, SCALAR_7]);

        let backstop_positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config.index, 50 * SCALAR_7)],
            supply: map![&e],
        };
        let filler_positions = Positions {
            collateral: map![&e, (reserve_config.index, 100 * SCALAR_7)],
            liabilities: map![&e],
            supply: map![&e],
        };
        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: SCALAR_7,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);
            storage::set_user_positions(&e, &filler, &filler_positions);
        });
        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let auction = new_bad_debt(&e, &pool_client, &backstop_address);

        assert_eq!(auction.bid.get(debt_asset.clone()), Some(50 * SCALAR_7));
        assert_eq!(auction.lot, map![&e, (lp_token.clone(), 60 * SCALAR_7)]);
        assert!(!pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());
        assert_eq!(
            pool_client.try_bad_debt(&backstop_address).err(),
            Some(Ok(Error::from_contract_error(1212)))
        );

        assert!(!try_fill_bad_debt(
            &e,
            &pool_client,
            &backstop_address,
            &filler,
            50
        ));
        e.ledger().set_sequence_number(auction.block + 100);
        let auction_before_failed_fill = get_bad_debt(&pool_client, &backstop_address);
        assert!(!try_fill_bad_debt(
            &e,
            &pool_client,
            &backstop_address,
            &unhealthy_filler,
            50
        ));
        assert_eq!(
            get_bad_debt(&pool_client, &backstop_address),
            auction_before_failed_fill
        );
        assert!(pool_client
            .get_positions(&unhealthy_filler)
            .liabilities
            .is_empty());

        fill_bad_debt(&e, &pool_client, &backstop_address, &filler, 50);
        assert_eq!(lp_token_client.balance(&filler), 15 * SCALAR_7);
        assert_eq!(
            pool_client
                .get_positions(&backstop_address)
                .liabilities
                .get(reserve_config.index),
            Some(25 * SCALAR_7)
        );
        assert_eq!(
            pool_client
                .get_positions(&filler)
                .liabilities
                .get(reserve_config.index),
            Some(25 * SCALAR_7)
        );
        assert_eq!(
            get_bad_debt(&pool_client, &backstop_address),
            AuctionData {
                bid: map![&e, (debt_asset.clone(), 25 * SCALAR_7)],
                lot: map![&e, (lp_token.clone(), 30 * SCALAR_7)],
                block: auction.block,
            }
        );
        e.ledger().set_sequence_number(auction.block + 500);
        del_bad_debt(&pool_client, &backstop_address);
        assert!(!e.as_contract(&pool_address, || {
            has_tier_auction(&e, AuctionType::BadDebtAuction)
        }));
        assert!(!pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());

        let discounted = new_bad_debt(&e, &pool_client, &backstop_address);
        e.ledger().set_sequence_number(discounted.block + 300);
        let filler_lp_before = lp_token_client.balance(&filler);
        let filler_debt_before = pool_client
            .get_positions(&filler)
            .liabilities
            .get(reserve_config.index)
            .unwrap();
        fill_bad_debt(&e, &pool_client, &backstop_address, &filler, 100);
        let filler_debt_after = pool_client
            .get_positions(&filler)
            .liabilities
            .get(reserve_config.index)
            .unwrap();
        assert_eq!(filler_debt_after - filler_debt_before, 12_5000000);
        assert_eq!(
            lp_token_client.balance(&filler) - filler_lp_before,
            30 * SCALAR_7
        );
        assert!(pool_client.try_get_auction(&1, &backstop_address).is_err());
        assert_eq!(
            pool_client
                .get_positions(&backstop_address)
                .liabilities
                .get(reserve_config.index),
            Some(12_5000000)
        );
        let continued = new_bad_debt(&e, &pool_client, &backstop_address);
        assert_eq!(continued.lot.keys().get(0), Some(lp_token.clone()));
        assert!(!pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());
        assert_eq!(
            continued.bid.get(debt_asset),
            Some(12_5000000),
            "the waterfall must reuse an eligible earlier tier"
        );
        assert!(pool_client.try_bad_debt(&backstop_address).is_err());

        e.ledger().set_sequence_number(continued.block + 200);
        fill_bad_debt(&e, &pool_client, &backstop_address, &filler, 100);
        assert!(pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());
    }

    #[test]
    fn bad_debt_creation_fails_closed_and_accepts_any_positive_tier() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();
        e.ledger().set(LedgerInfo {
            timestamp: 12_345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let admin = Address::generate(&e);
        let pool_address = create_pool(&e);
        let (blnd, _) = testutils::create_blnd_token(&e, &pool_address, &admin);
        let (usdc, _) = testutils::create_token_contract(&e, &admin);
        let (lp_token, _) = testutils::create_comet_lp_pool(&e, &admin, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // Keep a small positive tier balance so creation must obtain a valid
        // quote and use the tier before supplier default is allowed.
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &admin,
            &pool_address,
            &SCALAR_7,
        );
        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);
        let (debt_asset, _) = testutils::create_token_contract(&e, &admin);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.index = 0;
        reserve_data.last_time = e.ledger().timestamp();
        testutils::create_reserve(
            &e,
            &pool_address,
            &debt_asset,
            &reserve_config,
            &reserve_data,
        );
        oracle_client.set_data(
            &admin,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![&e, Asset::Stellar(debt_asset.clone())],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, SCALAR_7]);

        let debt = 50 * SCALAR_7;
        let backstop_positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config.index, debt)],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle: oracle_id,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0_1000000,
                    status: 0,
                    max_positions: 4,
                },
            );
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);
        });

        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let reserve_before = pool_client.get_reserve(&debt_asset).data;
        e.as_contract(&backstop_address, || {
            backstop::set_test_valuation_override(&e, Some(true));
        });
        assert!(pool_client
            .try_new_auction(&1, &backstop_address, &vec![&e], &vec![&e], &100)
            .is_err());
        let positions_after_failure = pool_client.get_positions(&backstop_address);
        assert_eq!(
            positions_after_failure.liabilities,
            backstop_positions.liabilities
        );
        assert_eq!(
            positions_after_failure.collateral,
            backstop_positions.collateral
        );
        assert_eq!(positions_after_failure.supply, backstop_positions.supply);
        let reserve_after_failure = pool_client.get_reserve(&debt_asset).data;
        assert_eq!(reserve_after_failure.b_rate, reserve_before.b_rate);
        assert_eq!(reserve_after_failure.b_supply, reserve_before.b_supply);
        assert_eq!(reserve_after_failure.d_rate, reserve_before.d_rate);
        assert_eq!(reserve_after_failure.d_supply, reserve_before.d_supply);
        assert!(pool_client.try_get_auction(&1, &backstop_address).is_err());

        e.as_contract(&backstop_address, || {
            backstop::set_test_valuation_override(&e, Some(false));
        });
        let auction = pool_client.new_auction(&1, &backstop_address, &vec![&e], &vec![&e], &100);
        assert_eq!(auction.bid.get(debt_asset), Some(debt));
        assert_eq!(auction.lot.len(), 1);
        assert!(pool_client.try_bad_debt(&backstop_address).is_err());
    }

    #[test]
    fn creation_caps_bid_and_rejects_unknown_liability_positions() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();
        e.ledger().set(LedgerInfo {
            timestamp: 12_345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let admin = Address::generate(&e);
        let depositor = Address::generate(&e);
        let pool_address = create_pool(&e);
        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &admin);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &admin);
        let (lp_token, lp_token_client) = testutils::create_comet_lp_pool(&e, &admin, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        blnd_client.mint(&depositor, &(5_001 * SCALAR_7));
        blnd_client.approve(&depositor, &lp_token, &i128::MAX, &99_999);
        usdc_client.mint(&depositor, &(126 * SCALAR_7));
        usdc_client.approve(&depositor, &lp_token, &i128::MAX, &99_999);
        lp_token_client.join_pool(
            &(500 * SCALAR_7),
            &vec![&e, 5_001 * SCALAR_7, 126 * SCALAR_7],
            &depositor,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &depositor,
            &pool_address,
            &(500 * SCALAR_7),
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);
        let mut assets = Vec::<Address>::new(&e);
        let mut oracle_assets = Vec::<Asset>::new(&e);
        let mut positions = Positions::env_default(&e);
        for index in 0..5_u32 {
            let (asset, _) = testutils::create_token_contract(&e, &admin);
            let (mut config, mut data) = testutils::default_reserve_meta();
            config.index = index;
            data.last_time = e.ledger().timestamp();
            testutils::create_reserve(&e, &pool_address, &asset, &config, &data);
            positions.liabilities.set(index, SCALAR_7);
            oracle_assets.push_back(Asset::Stellar(asset.clone()));
            assets.push_back(asset);
        }
        oracle_client.set_data(
            &admin,
            &Asset::Other(Symbol::new(&e, "USD")),
            &oracle_assets,
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, SCALAR_7, SCALAR_7, SCALAR_7, SCALAR_7, SCALAR_7]);
        e.as_contract(&pool_address, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle: oracle_id,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0_1000000,
                    status: 0,
                    max_positions: 6,
                },
            );
            storage::set_user_positions(&e, &backstop_address, &positions);
        });

        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let auction = new_bad_debt(&e, &pool_client, &backstop_address);
        assert_eq!(auction.bid.len(), MAX_BAD_DEBT_BID_ASSETS);
        for index in 0..MAX_BAD_DEBT_BID_ASSETS {
            assert_eq!(auction.bid.get(assets.get(index).unwrap()), Some(SCALAR_7));
        }
        assert_eq!(auction.bid.get(assets.get(4).unwrap()), None);

        e.ledger()
            .set_sequence_number(auction.block + super::super::AUCTION_STALE_LEDGERS);
        del_bad_debt(&pool_client, &backstop_address);
        let mut corrupt_positions = positions;
        corrupt_positions.liabilities.set(99, SCALAR_7);
        e.as_contract(&pool_address, || {
            e.storage().persistent().set(
                &crate::storage::PoolDataKey::Positions(backstop_address.clone()),
                &corrupt_positions,
            );
        });
        assert!(pool_client
            .try_new_auction(&1, &backstop_address, &vec![&e], &vec![&e], &100)
            .is_err());
        assert!(pool_client.try_get_auction(&1, &backstop_address).is_err());
    }
}
