use crate::{
    constants::SCALAR_7,
    dependencies::BackstopClient,
    errors::PoolError,
    pool::{Pool, User},
    storage,
};
use sep_41_token::TokenClient;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{contracttype, map, panic_with_error, Address, Env, Map, Vec};

use super::{
    math::{auction_modifiers, proportional_ceil, scale_bid_amount, scale_lot_amount},
    require_unique_addresses, AuctionData, AuctionType, AUCTION_STALE_LEDGERS,
};

const MAX_PROTOCOL_FEE_LOT_ASSETS: u32 = 4;
const PROTOCOL_FEE_AUCTION_MINIMUM_VALUE_USDC: i128 = 200 * SCALAR_7;

/// Create one independent protocol-fee auction whose filler bids BLNT for
/// reserve assets already recognized as protocol credit.
pub(crate) fn create_protocol_fee_auction_data(e: &Env, lot_assets: &Vec<Address>) -> AuctionData {
    let auction_type = AuctionType::ProtocolFeeAuction as u32;
    let backstop = storage::get_backstop(e);
    if storage::has_auction(e, &auction_type, &backstop) {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let mut pool = Pool::load(e);
    let max_lot_assets = core::cmp::min(
        pool.config
            .max_positions
            .checked_sub(1)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError)),
        MAX_PROTOCOL_FEE_LOT_ASSETS,
    );
    if lot_assets.is_empty() || lot_assets.len() > max_lot_assets {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    require_unique_addresses(e, lot_assets);

    let oracle_decimals = pool.load_price_decimals(e);
    let oracle_scalar = 10_i128
        .checked_pow(oracle_decimals)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let mut lot = Map::new(e);
    let mut lot_value = 0_i128;
    for asset in lot_assets {
        let reserve = pool.load_reserve(e, &asset, true);
        let protocol_fee = pool.protocol_fee_data(e, &asset);
        if protocol_fee.credit < 0 || !(0..SCALAR_7).contains(&protocol_fee.carry) {
            panic_with_error!(e, PoolError::BalanceError);
        }
        if reserve.is_authorized(e) && protocol_fee.credit > 0 {
            let price = pool.load_price(e, &asset);
            let value =
                value_reserve_amount(e, price, protocol_fee.credit, reserve.scalar, oracle_scalar);
            lot_value = lot_value
                .checked_add(value)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            lot.set(asset.clone(), protocol_fee.credit);
        }
        pool.cache_reserve(reserve);
    }
    if lot.is_empty() || lot_value < PROTOCOL_FEE_AUCTION_MINIMUM_VALUE_USDC {
        panic_with_error!(e, PoolError::NoInterestAuctionCapacity);
    }

    let blnt_price = BackstopClient::new(e, &backstop).blnt_price();
    if blnt_price <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let target_value = proportional_ceil(e, lot_value, 6, 5);
    let bid_amount = proportional_ceil(e, target_value, SCALAR_7, blnt_price);
    if bid_amount <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let blnt = storage::get_blnt_token(e);
    if TokenClient::new(e, &blnt).decimals() != 7 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let block = e
        .ledger()
        .sequence()
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let auction = AuctionData {
        bid: map![e, (blnt, bid_amount)],
        lot,
        block,
    };
    storage::set_auction(e, &auction_type, &backstop, &auction);
    pool.store_cached_reserves(e);
    auction
}

pub(crate) fn get_protocol_fee_auction(e: &Env) -> AuctionData {
    storage::get_auction(
        e,
        &(AuctionType::ProtocolFeeAuction as u32),
        &storage::get_backstop(e),
    )
}

pub(crate) fn fill_protocol_fee_auction(
    e: &Env,
    pool: &mut Pool,
    auction_user: &Address,
    filler_state: &User,
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
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }

    let auction = get_protocol_fee_auction(e);
    let fill = scale_protocol_fee_auction(e, &auction, percent);
    for (asset, amount) in fill.lot.iter() {
        let reserve = pool.load_reserve(e, &asset, true);
        let mut protocol_fee = pool.protocol_fee_data(e, &asset);
        protocol_fee.credit = protocol_fee
            .credit
            .checked_sub(amount)
            .filter(|value| *value >= 0)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::BalanceError));

        let token = TokenClient::new(e, &asset);
        let pool_before = token.balance(&pool_address);
        let filler_before = token.balance(&filler_state.address);
        token.transfer(&pool_address, &filler_state.address, &amount);
        if token.balance(&pool_address) != checked_sub(e, pool_before, amount)
            || token.balance(&filler_state.address) != checked_add(e, filler_before, amount)
        {
            panic_with_error!(e, PoolError::BalanceError);
        }
        pool.cache_reserve(reserve);
        pool.cache_protocol_fee_data(&asset, protocol_fee);
    }

    let blnt = storage::get_blnt_token(e);
    if fill.bid_amount > 0 {
        let token = TokenClient::new(e, &blnt);
        let pool_before = token.balance(&pool_address);
        let filler_before = token.balance(&filler_state.address);
        token.transfer(&filler_state.address, &pool_address, &fill.bid_amount);
        if token.balance(&pool_address) != checked_add(e, pool_before, fill.bid_amount)
            || token.balance(&filler_state.address)
                != checked_sub(e, filler_before, fill.bid_amount)
        {
            panic_with_error!(e, PoolError::BalanceError);
        }
        token.burn(&pool_address, &fill.bid_amount);
        if token.balance(&pool_address) != pool_before {
            panic_with_error!(e, PoolError::BalanceError);
        }
    }

    let auction_type = AuctionType::ProtocolFeeAuction as u32;
    if fill.complete {
        storage::del_auction(e, &auction_type, &backstop);
    } else {
        store_remaining_protocol_fee_auction(e, &auction, &fill);
    }
    AuctionData {
        bid: if fill.bid_amount > 0 {
            map![e, (blnt, fill.bid_amount)]
        } else {
            map![e]
        },
        lot: fill.lot,
        block: fill.block,
    }
}

pub(crate) fn delete_protocol_fee_auction(e: &Env) {
    let auction = get_protocol_fee_auction(e);
    let stale_at = auction
        .block
        .checked_add(AUCTION_STALE_LEDGERS)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if e.ledger().sequence() < stale_at && !lot_contains_deauthorized_reserve(e, &auction) {
        panic_with_error!(e, PoolError::BadRequest);
    }
    storage::del_auction(
        e,
        &(AuctionType::ProtocolFeeAuction as u32),
        &storage::get_backstop(e),
    );
}

/// Cancel a protocol-fee auction whose quoted lot was reduced by custody loss.
pub(crate) fn reconcile_protocol_credit(e: &Env, asset: &Address) -> bool {
    let auction_type = AuctionType::ProtocolFeeAuction as u32;
    let backstop = storage::get_backstop(e);
    if !storage::has_auction(e, &auction_type, &backstop) {
        return false;
    }
    let auction = storage::get_auction(e, &auction_type, &backstop);
    if !auction.lot.contains_key(asset.clone()) {
        return false;
    }
    storage::del_auction(e, &auction_type, &backstop);
    true
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
struct ProtocolFeeAuctionFill {
    base_bid_amount: i128,
    base_lot: Map<Address, i128>,
    bid_amount: i128,
    block: u32,
    complete: bool,
    lot: Map<Address, i128>,
}

fn scale_protocol_fee_auction(
    e: &Env,
    auction: &AuctionData,
    percent: u32,
) -> ProtocolFeeAuctionFill {
    if percent == 0 || percent > 100 || auction.bid.len() != 1 {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    let elapsed = e
        .ledger()
        .sequence()
        .checked_sub(auction.block)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::InvalidInterestAuction));
    let (bid_modifier, lot_modifier) = auction_modifiers(e, elapsed);
    let percent_scaled = i128::from(percent)
        .checked_mul(100_000)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let (_, bid_amount) = auction.bid.iter().next().unwrap();
    let (base_bid_amount, bid_amount, remaining_bid) =
        scale_bid_amount(e, bid_amount, percent_scaled, bid_modifier);
    let mut base_lot = Map::new(e);
    let mut lot = Map::new(e);
    for (asset, amount) in auction.lot.iter() {
        let (base, actual, _) = scale_lot_amount(e, amount, percent_scaled, lot_modifier);
        if base > 0 {
            base_lot.set(asset.clone(), base);
        }
        if actual > 0 {
            lot.set(asset, actual);
        }
    }
    ProtocolFeeAuctionFill {
        base_bid_amount,
        base_lot,
        bid_amount,
        block: auction.block,
        complete: remaining_bid == 0,
        lot,
    }
}

fn store_remaining_protocol_fee_auction(
    e: &Env,
    auction: &AuctionData,
    fill: &ProtocolFeeAuctionFill,
) {
    let bid_token = auction.bid.keys().get(0).unwrap();
    let remaining_bid = checked_sub(
        e,
        auction.bid.get(bid_token.clone()).unwrap(),
        fill.base_bid_amount,
    );
    if remaining_bid <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let mut remaining_lot = Map::new(e);
    for (asset, amount) in auction.lot.iter() {
        let remainder = checked_sub(e, amount, fill.base_lot.get(asset.clone()).unwrap_or(0));
        if remainder > 0 {
            remaining_lot.set(asset, remainder);
        }
    }
    storage::set_auction(
        e,
        &(AuctionType::ProtocolFeeAuction as u32),
        &storage::get_backstop(e),
        &AuctionData {
            bid: map![e, (bid_token, remaining_bid)],
            lot: remaining_lot,
            block: auction.block,
        },
    );
}

fn lot_contains_deauthorized_reserve(e: &Env, auction: &AuctionData) -> bool {
    let mut pool = Pool::load(e);
    for asset in auction.lot.keys() {
        if !pool.load_reserve(e, &asset, false).is_authorized(e) {
            return true;
        }
    }
    false
}

fn value_reserve_amount(
    e: &Env,
    price: i128,
    amount: i128,
    reserve_scalar: i128,
    oracle_scalar: i128,
) -> i128 {
    if price <= 0 || amount < 0 || reserve_scalar <= 0 || oracle_scalar <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let oracle_value = price.fixed_mul_floor(e, &amount, &reserve_scalar);
    oracle_value.fixed_mul_floor(e, &SCALAR_7, &oracle_scalar)
}

fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

fn checked_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

#[cfg(test)]
mod tests {
    use crate::{
        auctions::{has_tier_auction, set_tier_auction, BackstopTier, TierAuctionData},
        storage::{PoolConfig, ProtocolFeeData},
        testutils,
    };
    use sep_40_oracle::testutils::Asset;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        vec, Symbol,
    };

    use super::*;

    #[test]
    fn protocol_fee_auction_bids_blnt_and_coexists_with_interest_auction() {
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
        let filler = Address::generate(&e);
        let pool_address = testutils::create_pool(&e);
        let (blnt, blnt_client) = testutils::create_blnt_token(&e, &pool_address, &admin);
        let (usdc, _) = testutils::create_token_contract(&e, &admin);
        let (blnt_usdc, _) = testutils::create_comet_lp_pool(&e, &admin, &blnt, &usdc);
        let (backstop, _) = testutils::create_backstop(&e, &pool_address, &blnt_usdc, &usdc, &blnt);
        let (oracle, oracle_client) = testutils::create_mock_oracle(&e);
        let (reserve_asset, reserve_token) = testutils::create_token_contract(&e, &admin);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.index = 0;
        reserve_data.b_supply = 0;
        reserve_data.d_supply = 0;
        reserve_data.last_time = 12_345;
        testutils::create_reserve(
            &e,
            &pool_address,
            &reserve_asset,
            &reserve_config,
            &reserve_data,
        );
        oracle_client.set_data(
            &admin,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![&e, Asset::Stellar(reserve_asset.clone())],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, SCALAR_7]);
        reserve_token.mint(&pool_address, &(200 * SCALAR_7));
        blnt_client.mint(&filler, &(2_400 * SCALAR_7));

        e.as_contract(&pool_address, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0,
                    status: 0,
                    max_positions: 4,
                },
            );
            storage::set_protocol_fee_data(
                &e,
                &reserve_asset,
                &ProtocolFeeData {
                    credit: 200 * SCALAR_7,
                    carry: 0,
                },
            );
            set_tier_auction(
                &e,
                AuctionType::InterestAuction,
                &TierAuctionData {
                    auction: AuctionData {
                        bid: map![&e, (Address::generate(&e), SCALAR_7)],
                        lot: map![&e, (reserve_asset.clone(), SCALAR_7)],
                        block: 51,
                    },
                    tier: BackstopTier::FirstLoss,
                },
            );

            let auction = create_protocol_fee_auction_data(&e, &vec![&e, reserve_asset.clone()]);
            assert_eq!(auction.bid, map![&e, (blnt.clone(), 2_400 * SCALAR_7)]);
            assert_eq!(
                auction.lot,
                map![&e, (reserve_asset.clone(), 200 * SCALAR_7)]
            );
            assert_eq!(auction.block, 51);
            assert!(has_tier_auction(&e, AuctionType::InterestAuction));
            assert!(storage::has_auction(
                &e,
                &(AuctionType::ProtocolFeeAuction as u32),
                &backstop
            ));

            e.ledger().set(LedgerInfo {
                timestamp: 13_345,
                protocol_version: 27,
                sequence_number: 251,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 10,
                min_persistent_entry_ttl: 10,
                max_entry_ttl: 3_110_400,
            });
            let mut pool = Pool::load(&e);
            let filler_state = User::load(&e, &filler);
            let fill = fill_protocol_fee_auction(&e, &mut pool, &backstop, &filler_state, 100);
            pool.store_cached_reserves(&e);

            assert_eq!(fill.bid, map![&e, (blnt.clone(), 2_400 * SCALAR_7)]);
            assert_eq!(fill.lot, map![&e, (reserve_asset.clone(), 200 * SCALAR_7)]);
            assert_eq!(reserve_token.balance(&filler), 200 * SCALAR_7);
            assert_eq!(blnt_client.balance(&filler), 0);
            assert_eq!(blnt_client.balance(&pool_address), 0);
            assert_eq!(storage::get_protocol_fee_data(&e, &reserve_asset).credit, 0);
            assert!(!storage::has_auction(
                &e,
                &(AuctionType::ProtocolFeeAuction as u32),
                &backstop
            ));
            assert!(has_tier_auction(&e, AuctionType::InterestAuction));
        });
    }
}
