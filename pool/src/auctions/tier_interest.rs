use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopContractAsset, BackstopPoolData, BackstopPoolTierData},
    errors::PoolError,
    pool::{Pool, User},
    storage,
};
use sep_41_token::TokenClient;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Map, Vec};

use super::{
    math::{
        auction_modifiers, proportional_ceil, proportional_floor, scale_bid_amount,
        scale_lot_amount,
    },
    require_unique_addresses, to_backstop_tier, AuctionData, BackstopTier, AUCTION_STALE_LEDGERS,
};

const ONE_DAY_LEDGERS: u32 = 17_280;
const MAX_INTEREST_LOT_ASSETS: u32 = 4;
const INTEREST_AUCTION_MINIMUM_VALUE_USDC: i128 = 200 * SCALAR_7;
const INTEREST_AUCTION_TTL_THRESHOLD: u32 = 45 * ONE_DAY_LEDGERS;
const INTEREST_AUCTION_TTL_BUMP: u32 = 46 * ONE_DAY_LEDGERS;
const INTEREST_STATE_TTL_THRESHOLD: u32 = 179 * ONE_DAY_LEDGERS;
const INTEREST_STATE_TTL_BUMP: u32 = 180 * ONE_DAY_LEDGERS;

pub(crate) fn create_interest_auction_data(e: &Env, lot_assets: &Vec<Address>) -> AuctionData {
    if has_interest_auction(e) {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }
    let mut pool = Pool::load(e);
    let max_lot_assets = core::cmp::min(
        pool.config
            .max_positions
            .checked_sub(1)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError)),
        MAX_INTEREST_LOT_ASSETS,
    );
    if lot_assets.is_empty() || lot_assets.len() > max_lot_assets {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    require_unique_addresses(e, lot_assets);

    let oracle_decimals = pool.load_price_decimals(e);
    let oracle_scalar = 10_i128
        .checked_pow(oracle_decimals)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let mut states = Map::<Address, InterestReserveState>::new(e);
    let mut distributions = Map::<Address, i128>::new(e);
    for asset in lot_assets {
        let reserve = pool.load_reserve(e, &asset, true);
        let mut state = get_interest_reserve_state(e, &asset);
        let uncheckpointed = checked_sub(e, reserve.data.backstop_credit, state.total(e));
        let distribution = checked_add(e, uncheckpointed, state.carry);
        state.carry = 0;
        states.set(asset.clone(), state);
        distributions.set(asset.clone(), distribution);
        pool.cache_reserve(reserve);
    }

    let backstop = storage::get_backstop(e);
    let pool_address = e.current_contract_address();
    let backstop_client = BackstopClient::new(e, &backstop);
    let pool_data = backstop_client.pool_data(&pool_address);
    for (asset, distribution) in distributions.iter() {
        let allocation = allocate_take_rate(e, distribution, &pool_data.tiers);
        apply_take_rate_allocation(e, &mut states, &asset, allocation);
    }

    let mut tier_values = Vec::new(e);
    for _ in 0..pool_data.tiers.len() {
        tier_values.push_back(0_i128);
    }
    for asset in lot_assets {
        let state = states.get(asset.clone()).unwrap();
        let reserve = pool.load_reserve(e, &asset, false);
        if !reserve.is_authorized(e) {
            continue;
        }
        let price = pool.load_price(e, &asset);
        for index in 0..pool_data.tiers.len() {
            if pool_data.tiers.get(index).unwrap().value == 0 {
                continue;
            }
            let value = value_reserve_amount(
                e,
                price,
                state.tier_amount(tier_from_index(e, index)),
                reserve.scalar,
                oracle_scalar,
            );
            tier_values.set(
                index,
                checked_add(e, tier_values.get(index).unwrap(), value),
            );
        }
    }

    let selected = select_interest_group(e, interest_tier_cursor(e), &tier_values, &pool_data);
    if TokenClient::new(e, &selected.bid_token).decimals() != 7 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let mut tiers = Vec::new(e);
    for index in selected.tier_indices.iter() {
        let tier = tier_from_index(e, index);
        let lot_value = tier_values.get(index).unwrap();
        let mut lot = Map::new(e);
        for asset in lot_assets {
            let state = states.get(asset.clone()).unwrap();
            let reserve = pool.load_reserve(e, &asset, false);
            if reserve.is_authorized(e) {
                let amount = state.tier_amount(tier);
                if amount > 0 {
                    lot.set(asset.clone(), amount);
                }
            }
        }
        if lot.is_empty() {
            panic_with_error!(e, PoolError::NoInterestAuctionCapacity);
        }
        let bid_amount = build_interest_bid(e, &pool_data.tiers.get(index).unwrap(), lot_value);
        tiers.push_back(InterestTierAuctionData {
            bid_amount,
            lot,
            tier,
        });
    }
    for asset in lot_assets {
        set_interest_reserve_state(e, &asset, &states.get(asset.clone()).unwrap());
    }
    let block = e
        .ledger()
        .sequence()
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let auction = InterestAuctionData {
        bid_token: selected.bid_token,
        block,
        tiers,
    };
    let public_auction = project_interest_auction(e, &auction);
    set_interest_auction(e, &auction);
    e.storage()
        .instance()
        .set(&InterestDataKey::Cursor, &selected.next_cursor);
    pool.store_cached_reserves(e);
    public_auction
}

pub(crate) fn get_interest_auction(e: &Env) -> AuctionData {
    project_interest_auction(e, &get_interest_auction_data(e))
}

pub(crate) fn fill_interest_auction(
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

    let auction = get_interest_auction_data(e);
    let fill = scale_interest_auction(e, &auction, percent);
    for (asset, amount) in fill.lot.iter() {
        let mut reserve = pool.load_reserve(e, &asset, true);
        let token = TokenClient::new(e, &asset);
        let pool_before = token.balance(&pool_address);
        let filler_before = token.balance(&filler_state.address);
        token.transfer(&pool_address, &filler_state.address, &amount);
        if token.balance(&pool_address) != checked_sub(e, pool_before, amount)
            || token.balance(&filler_state.address) != checked_add(e, filler_before, amount)
        {
            panic_with_error!(e, PoolError::BalanceError);
        }
        reserve.data.backstop_credit = checked_sub(e, reserve.data.backstop_credit, amount);
        pool.cache_reserve(reserve);
    }
    let backstop_client = BackstopClient::new(e, &backstop);
    for index in 0..auction.tiers.len() {
        let tier = auction.tiers.get(index).unwrap().tier;
        let slot = index as usize;
        consume_pending_interest_lot(e, tier, &fill.lots[slot]);
        if fill.bids[slot] > 0 {
            backstop_client.donate(
                &to_backstop_tier(tier),
                &filler_state.address,
                &pool_address,
                &fill.bids[slot],
            );
        }
    }
    if fill.complete {
        remove_interest_auction(e);
    } else {
        store_remaining_interest_auction(e, &auction, &fill);
    }
    let bid = if fill.bid_amount > 0 {
        soroban_sdk::map![e, (auction.bid_token, fill.bid_amount)]
    } else {
        soroban_sdk::map![e]
    };
    AuctionData {
        bid,
        lot: fill.lot,
        block: fill.block,
    }
}

/// One tier's immutable base contribution to an active interest auction.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub(crate) struct InterestTierAuctionData {
    pub bid_amount: i128,
    pub lot: Map<Address, i128>,
    pub tier: BackstopTier,
}

/// Private interest-auction metadata. All tiers use the same bid token.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub(crate) struct InterestAuctionData {
    pub bid_token: Address,
    pub block: u32,
    pub tiers: Vec<InterestTierAuctionData>,
}

/// Pending reserve credit apportioned to each tier for one reserve asset.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
struct InterestReserveState {
    carry: i128,
    first_loss: i128,
    second_loss: i128,
    third_loss: i128,
}

impl InterestReserveState {
    fn empty() -> Self {
        Self {
            carry: 0,
            first_loss: 0,
            second_loss: 0,
            third_loss: 0,
        }
    }

    fn total(&self, e: &Env) -> i128 {
        checked_add(
            e,
            checked_add(e, self.first_loss, self.second_loss),
            checked_add(e, self.third_loss, self.carry),
        )
    }

    fn tier_amount(&self, tier: super::BackstopTier) -> i128 {
        match tier {
            super::BackstopTier::FirstLoss => self.first_loss,
            super::BackstopTier::SecondLoss => self.second_loss,
            super::BackstopTier::ThirdLoss => self.third_loss,
        }
    }

    fn set_tier_amount(&mut self, tier: super::BackstopTier, amount: i128) {
        match tier {
            super::BackstopTier::FirstLoss => self.first_loss = amount,
            super::BackstopTier::SecondLoss => self.second_loss = amount,
            super::BackstopTier::ThirdLoss => self.third_loss = amount,
        }
    }
}

/// Aggregated public amounts and exact tier-local settlement amounts.
struct InterestAuctionFill {
    bid_amount: i128,
    bids: [i128; 3],
    block: u32,
    complete: bool,
    lot: Map<Address, i128>,
    lots: [Map<Address, i128>; 3],
    remaining_bids: [i128; 3],
    remaining_lots: [Map<Address, i128>; 3],
}

#[derive(Clone)]
#[contracttype(export = false)]
enum InterestDataKey {
    Cursor,
    Reserve(Address),
    Auction,
}

pub(crate) fn has_interest_auction(e: &Env) -> bool {
    e.storage().temporary().has(&InterestDataKey::Auction)
}

pub(crate) fn get_interest_auction_data(e: &Env) -> InterestAuctionData {
    e.storage()
        .temporary()
        .get(&InterestDataKey::Auction)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest))
}

pub(crate) fn set_interest_auction(e: &Env, auction: &InterestAuctionData) {
    if auction.tiers.is_empty() {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    e.storage()
        .temporary()
        .set(&InterestDataKey::Auction, auction);
    e.storage().temporary().extend_ttl(
        &InterestDataKey::Auction,
        INTEREST_AUCTION_TTL_THRESHOLD,
        INTEREST_AUCTION_TTL_BUMP,
    );
}

pub(crate) fn remove_interest_auction(e: &Env) {
    e.storage().temporary().remove(&InterestDataKey::Auction);
}

fn get_interest_reserve_state(e: &Env, asset: &Address) -> InterestReserveState {
    let key = InterestDataKey::Reserve(asset.clone());
    let state = e
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(InterestReserveState::empty);
    if e.storage().persistent().has(&key) {
        e.storage().persistent().extend_ttl(
            &key,
            INTEREST_STATE_TTL_THRESHOLD,
            INTEREST_STATE_TTL_BUMP,
        );
    }
    state
}

fn set_interest_reserve_state(e: &Env, asset: &Address, state: &InterestReserveState) {
    if state.first_loss < 0 || state.second_loss < 0 || state.third_loss < 0 || state.carry < 0 {
        panic_with_error!(e, PoolError::OverflowError);
    }
    let key = InterestDataKey::Reserve(asset.clone());
    e.storage().persistent().set(&key, state);
    e.storage().persistent().extend_ttl(
        &key,
        INTEREST_STATE_TTL_THRESHOLD,
        INTEREST_STATE_TTL_BUMP,
    );
}

/// Reduce one reserve's unpaid take-rate accounting after a direct custody
/// loss. Stored tier amounts and carry are reduced proportionally with the
/// total credit. Any active interest auction containing the reserve is
/// canceled because its underlying-token lot was quoted before the loss.
///
/// Returns true when an active interest auction was canceled.
pub(crate) fn reconcile_interest_credit(
    e: &Env,
    asset: &Address,
    previous_credit: i128,
    new_credit: i128,
) -> bool {
    if previous_credit <= 0 || new_credit < 0 || new_credit >= previous_credit {
        panic_with_error!(e, PoolError::BalanceError);
    }

    let auction_canceled = if has_interest_auction(e) {
        let auction = get_interest_auction_data(e);
        if interest_auction_contains_asset(&auction, asset) {
            remove_interest_auction(e);
            true
        } else {
            false
        }
    } else {
        false
    };

    let mut state = get_interest_reserve_state(e, asset);
    if state.total(e) > previous_credit {
        panic_with_error!(e, PoolError::BalanceError);
    }
    state.first_loss = proportional_floor(e, state.first_loss, new_credit, previous_credit);
    state.second_loss = proportional_floor(e, state.second_loss, new_credit, previous_credit);
    state.third_loss = proportional_floor(e, state.third_loss, new_credit, previous_credit);
    state.carry = proportional_floor(e, state.carry, new_credit, previous_credit);
    set_interest_reserve_state(e, asset, &state);

    auction_canceled
}

fn consume_pending_interest_lot(
    e: &Env,
    tier: super::BackstopTier,
    transferred: &Map<Address, i128>,
) {
    for (asset, amount) in transferred.iter() {
        let mut state = get_interest_reserve_state(e, &asset);
        state.set_tier_amount(tier, checked_sub(e, state.tier_amount(tier), amount));
        set_interest_reserve_state(e, &asset, &state);
    }
}

fn allocate_take_rate(
    e: &Env,
    distribution: i128,
    tiers: &Vec<crate::dependencies::BackstopPoolTierData>,
) -> InterestReserveState {
    if distribution < 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let mut weighted_values = Vec::new(e);
    let mut denominator = 0_i128;
    for tier in tiers.iter() {
        if tier.value < 0 || tier.take_rate_weight == 0 {
            panic_with_error!(e, PoolError::InvalidLot);
        }
        let weighted = checked_mul(e, tier.value, i128::from(tier.take_rate_weight));
        denominator = checked_add(e, denominator, weighted);
        weighted_values.push_back(weighted);
    }
    if denominator == 0 {
        return InterestReserveState {
            carry: distribution,
            first_loss: 0,
            second_loss: 0,
            third_loss: 0,
        };
    }
    let mut result = InterestReserveState::empty();
    let mut allocated = 0_i128;
    for index in 0..weighted_values.len() {
        let amount = proportional_floor(
            e,
            distribution,
            weighted_values.get(index).unwrap(),
            denominator,
        );
        result.set_tier_amount(tier_from_index(e, index), amount);
        allocated = checked_add(e, allocated, amount);
    }
    result.carry = checked_sub(e, distribution, allocated);
    result
}

fn apply_take_rate_allocation(
    e: &Env,
    states: &mut Map<Address, InterestReserveState>,
    asset: &Address,
    allocation: InterestReserveState,
) {
    let mut state = states.get(asset.clone()).unwrap();
    state.first_loss = checked_add(e, state.first_loss, allocation.first_loss);
    state.second_loss = checked_add(e, state.second_loss, allocation.second_loss);
    state.third_loss = checked_add(e, state.third_loss, allocation.third_loss);
    state.carry = allocation.carry;
    states.set(asset.clone(), state);
}

fn build_interest_bid(e: &Env, tier_data: &BackstopPoolTierData, lot_value: i128) -> i128 {
    let tokens = tier_data.tokens;
    let shares = tier_data.shares;
    let value = tier_data.value;
    if lot_value <= 0 || tokens <= 0 || shares <= 0 || value <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }

    let target_value = proportional_ceil(e, lot_value, 6, 5);
    let bid_amount = proportional_ceil(e, target_value, tokens, value);
    if bid_amount <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    bid_amount
}

fn value_reserve_amount(
    e: &Env,
    price: i128,
    amount: i128,
    reserve_scalar: i128,
    oracle_scalar: i128,
) -> i128 {
    if amount < 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let oracle_value = price.fixed_mul_floor(e, &amount, &reserve_scalar);
    oracle_value.fixed_mul_floor(e, &SCALAR_7, &oracle_scalar)
}

fn project_interest_auction(e: &Env, auction: &InterestAuctionData) -> AuctionData {
    let mut bid_amount = 0_i128;
    let mut lot = Map::new(e);
    for tier in auction.tiers.iter() {
        bid_amount = checked_add(e, bid_amount, tier.bid_amount);
        add_amounts(e, &mut lot, &tier.lot);
    }
    if bid_amount <= 0 {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    AuctionData {
        bid: soroban_sdk::map![e, (auction.bid_token.clone(), bid_amount)],
        lot,
        block: auction.block,
    }
}

fn add_amounts(e: &Env, total: &mut Map<Address, i128>, amounts: &Map<Address, i128>) {
    for (asset, amount) in amounts.iter() {
        if amount <= 0 {
            panic_with_error!(e, PoolError::InvalidLot);
        }
        let next = checked_add(e, total.get(asset.clone()).unwrap_or(0), amount);
        total.set(asset, next);
    }
}

fn store_remaining_interest_auction(
    e: &Env,
    auction: &InterestAuctionData,
    fill: &InterestAuctionFill,
) {
    if fill.complete {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    let mut remaining_tiers = Vec::new(e);
    for index in 0..auction.tiers.len() {
        let tier = auction.tiers.get(index).unwrap();
        let slot = index as usize;
        let remaining_bid = fill.remaining_bids[slot];
        let remaining_lot = fill.remaining_lots[slot].clone();
        // Bid uses ceiling rounding while lot uses floor rounding. Preserve a
        // tier contribution until both sides are consumed so the public
        // remainder exactly matches the aggregate auction's base reduction.
        if remaining_bid > 0 || !remaining_lot.is_empty() {
            remaining_tiers.push_back(InterestTierAuctionData {
                bid_amount: remaining_bid,
                lot: remaining_lot,
                tier: tier.tier,
            });
        }
    }
    set_interest_auction(
        e,
        &InterestAuctionData {
            bid_token: auction.bid_token.clone(),
            block: auction.block,
            tiers: remaining_tiers,
        },
    );
}

fn scale_interest_auction(
    e: &Env,
    auction: &InterestAuctionData,
    percent: u32,
) -> InterestAuctionFill {
    if percent == 0 || percent > 100 || auction.tiers.is_empty() || auction.tiers.len() > 3 {
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
    let public = project_interest_auction(e, auction);
    let public_bid = public.bid.get(auction.bid_token.clone()).unwrap();
    let (base_bid, bid_amount, remaining_bid) =
        scale_bid_amount(e, public_bid, percent_scaled, bid_modifier);
    let tier_count = auction.tiers.len();
    let mut bid_weights = [0_i128; 3];
    for index in 0..tier_count {
        bid_weights[index as usize] = auction.tiers.get(index).unwrap().bid_amount;
    }
    let base_bids = proportional_allocations(e, base_bid, &bid_weights, tier_count);
    let bids = proportional_allocations(e, bid_amount, &base_bids, tier_count);
    let mut remaining_bids = [0_i128; 3];
    for index in 0..tier_count {
        let slot = index as usize;
        remaining_bids[slot] = checked_sub(e, bid_weights[slot], base_bids[slot]);
    }

    let mut lots = [Map::new(e), Map::new(e), Map::new(e)];
    let mut remaining_lots = [Map::new(e), Map::new(e), Map::new(e)];
    let mut lot = Map::new(e);
    for (asset, amount) in public.lot.iter() {
        let (base, actual, _) = scale_lot_amount(e, amount, percent_scaled, lot_modifier);
        let mut weights = [0_i128; 3];
        for index in 0..tier_count {
            weights[index as usize] = auction
                .tiers
                .get(index)
                .unwrap()
                .lot
                .get(asset.clone())
                .unwrap_or(0);
        }
        let base_allocations = proportional_allocations(e, base, &weights, tier_count);
        let allocations = proportional_allocations(e, actual, &base_allocations, tier_count);
        for index in 0..tier_count {
            let slot = index as usize;
            let base_amount = base_allocations[slot];
            let remaining_amount = checked_sub(e, weights[slot], base_amount);
            if remaining_amount > 0 {
                remaining_lots[slot].set(asset.clone(), remaining_amount);
            }
            let actual_amount = allocations[slot];
            if actual_amount > 0 {
                lots[slot].set(asset.clone(), actual_amount);
            }
        }
        if actual > 0 {
            lot.set(asset, actual);
        }
    }

    InterestAuctionFill {
        bid_amount,
        bids,
        block: auction.block,
        complete: remaining_bid == 0,
        lot,
        lots,
        remaining_bids,
        remaining_lots,
    }
}

fn proportional_allocations(e: &Env, total: i128, weights: &[i128; 3], length: u32) -> [i128; 3] {
    if total < 0 || length == 0 || length > 3 {
        panic_with_error!(e, PoolError::OverflowError);
    }
    let mut weight_total = 0_i128;
    for index in 0..length {
        weight_total = checked_add(e, weight_total, weights[index as usize]);
    }
    if total > weight_total || (total > 0 && weight_total == 0) {
        panic_with_error!(e, PoolError::OverflowError);
    }
    let mut allocations = [0_i128; 3];
    let mut allocated = 0_i128;
    for index in 0..length {
        let slot = index as usize;
        let amount = if weight_total == 0 {
            0
        } else {
            proportional_floor(e, total, weights[slot], weight_total)
        };
        allocated = checked_add(e, allocated, amount);
        allocations[slot] = amount;
    }
    let mut remainder = checked_sub(e, total, allocated);
    for index in 0..length {
        if remainder == 0 {
            break;
        }
        let slot = index as usize;
        if allocations[slot] < weights[slot] {
            allocations[slot] = checked_add(e, allocations[slot], 1);
            remainder = checked_sub(e, remainder, 1);
        }
    }
    if remainder != 0 {
        panic_with_error!(e, PoolError::OverflowError);
    }
    allocations
}

pub(crate) fn delete_interest_auction(e: &Env) {
    let auction = get_interest_auction_data(e);
    let stale_at = auction
        .block
        .checked_add(AUCTION_STALE_LEDGERS)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if e.ledger().sequence() < stale_at
        && !selected_usdc_group_has_no_transferable_value(e, &auction)
        && !interest_lot_contains_deauthorized_reserve(e, &auction)
    {
        panic_with_error!(e, PoolError::BadRequest);
    }
    remove_interest_auction(e);
}

fn interest_lot_contains_deauthorized_reserve(e: &Env, auction: &InterestAuctionData) -> bool {
    let mut pool = Pool::load(e);
    for tier in auction.tiers.iter() {
        for asset in tier.lot.keys() {
            if !pool.load_reserve(e, &asset, false).is_authorized(e) {
                return true;
            }
        }
    }
    false
}

fn selected_usdc_group_has_no_transferable_value(e: &Env, auction: &InterestAuctionData) -> bool {
    let backstop = storage::get_backstop(e);
    let pool_data = BackstopClient::new(e, &backstop).pool_data(&e.current_contract_address());
    for selected in auction.tiers.iter() {
        let tier = pool_data
            .tiers
            .get(tier_index(selected.tier))
            .unwrap_or_else(|| panic_with_error!(e, PoolError::InvalidLot));
        if matches!(tier.asset, BackstopContractAsset::Usdc) && tier.tokens > 0 && tier.value == 0 {
            return true;
        }
    }
    false
}

fn interest_auction_contains_asset(auction: &InterestAuctionData, asset: &Address) -> bool {
    for tier in auction.tiers.iter() {
        if tier.lot.contains_key(asset.clone()) {
            return true;
        }
    }
    false
}

fn interest_tier_cursor(e: &Env) -> u32 {
    let cursor = e
        .storage()
        .instance()
        .get(&InterestDataKey::Cursor)
        .unwrap_or(0);
    if cursor > 2 {
        panic_with_error!(e, PoolError::OverflowError);
    }
    cursor
}

fn tier_from_index(e: &Env, index: u32) -> super::BackstopTier {
    match index {
        0 => super::BackstopTier::FirstLoss,
        1 => super::BackstopTier::SecondLoss,
        2 => super::BackstopTier::ThirdLoss,
        _ => panic_with_error!(e, PoolError::InvalidLot),
    }
}

struct SelectedInterestGroup {
    bid_token: Address,
    next_cursor: u32,
    tier_indices: Vec<u32>,
}

fn select_interest_group(
    e: &Env,
    cursor: u32,
    tier_values: &Vec<i128>,
    pool_data: &BackstopPoolData,
) -> SelectedInterestGroup {
    if tier_values.is_empty()
        || tier_values.len() != pool_data.tiers.len()
        || cursor >= tier_values.len()
    {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let mut visited = Map::<Address, bool>::new(e);
    for offset in 0..tier_values.len() {
        let index = (cursor + offset) % tier_values.len();
        let token = pool_data.tiers.get(index).unwrap().token;
        if visited.contains_key(token.clone()) {
            continue;
        }
        visited.set(token.clone(), true);
        let mut value = 0_i128;
        let mut tier_indices = Vec::new(e);
        for candidate_index in 0..tier_values.len() {
            let candidate = pool_data.tiers.get(candidate_index).unwrap();
            let tier_value = tier_values.get(candidate_index).unwrap();
            if candidate.token == token && candidate.value > 0 && tier_value > 0 {
                value = checked_add(e, value, tier_value);
                tier_indices.push_back(candidate_index);
            }
        }
        if value >= INTEREST_AUCTION_MINIMUM_VALUE_USDC {
            return SelectedInterestGroup {
                bid_token: token.clone(),
                next_cursor: next_interest_group_cursor(e, pool_data, index, &token),
                tier_indices,
            };
        }
    }
    panic_with_error!(e, PoolError::NoInterestAuctionCapacity)
}

fn next_interest_group_cursor(
    e: &Env,
    pool_data: &BackstopPoolData,
    selected_index: u32,
    selected_token: &Address,
) -> u32 {
    let length = pool_data.tiers.len();
    if length == 0 || selected_index >= length {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    for offset in 1..length {
        let index = (selected_index + offset) % length;
        if pool_data.tiers.get(index).unwrap().token != *selected_token {
            return index;
        }
    }
    0
}

fn tier_index(tier: super::BackstopTier) -> u32 {
    match tier {
        super::BackstopTier::FirstLoss => 0,
        super::BackstopTier::SecondLoss => 1,
        super::BackstopTier::ThirdLoss => 2,
    }
}

fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

fn checked_mul(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

fn checked_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .filter(|result| *result >= 0)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec};

    use crate::{
        auctions::{has_tier_auction, AuctionType},
        testutils::create_pool,
    };

    use super::*;

    fn tier_data(e: &Env, value: i128, weight: u32) -> crate::dependencies::BackstopPoolTierData {
        tier_data_with_token(e, Address::generate(e), value, weight)
    }

    fn tier_data_with_token(
        _e: &Env,
        token: Address,
        value: i128,
        weight: u32,
    ) -> crate::dependencies::BackstopPoolTierData {
        crate::dependencies::BackstopPoolTierData {
            asset: crate::dependencies::BackstopContractAsset::BlntXlm,
            blnt_emission_eligible: false,
            take_rate_weight: weight,
            token,
            tokens: value,
            shares: value,
            value,
        }
    }

    fn pool_data(
        _e: &Env,
        tiers: Vec<crate::dependencies::BackstopPoolTierData>,
    ) -> BackstopPoolData {
        let mut active_value = 0_i128;
        for tier in tiers.iter() {
            active_value += tier.value;
        }
        BackstopPoolData {
            active_value,
            q4w_pct: 0,
            tiers,
        }
    }

    #[test]
    fn take_rate_allocation_applies_pool_weights() {
        let e = Env::default();
        let allocation = allocate_take_rate(
            &e,
            90,
            &vec![
                &e,
                tier_data(&e, 1, 4),
                tier_data(&e, 1, 3),
                tier_data(&e, 1, 2),
            ],
        );

        assert_eq!(
            allocation,
            InterestReserveState {
                carry: 0,
                first_loss: 40,
                second_loss: 30,
                third_loss: 20,
            }
        );
    }

    #[test]
    fn take_rate_allocation_conserves_rounding_remainder() {
        let e = Env::default();
        let allocation = allocate_take_rate(
            &e,
            10,
            &vec![
                &e,
                tier_data(&e, 3, 1),
                tier_data(&e, 2, 1),
                tier_data(&e, 1, 1),
            ],
        );

        assert_eq!(
            allocation.first_loss
                + allocation.second_loss
                + allocation.third_loss
                + allocation.carry,
            10
        );
        assert_eq!(allocation.carry, 1);
    }

    #[test]
    fn take_rate_allocation_renormalizes_around_zero_value_tier() {
        let e = Env::default();
        let allocation = allocate_take_rate(
            &e,
            60,
            &vec![
                &e,
                tier_data(&e, 1, 4),
                tier_data(&e, 0, 3),
                tier_data(&e, 1, 2),
            ],
        );

        assert_eq!(allocation.first_loss, 40);
        assert_eq!(allocation.second_loss, 0);
        assert_eq!(allocation.third_loss, 20);
        assert_eq!(allocation.carry, 0);
    }

    #[test]
    fn interest_threshold_includes_exactly_two_hundred_usdc() {
        let e = Env::default();
        let first = Address::generate(&e);
        let second = Address::generate(&e);
        let third = Address::generate(&e);
        let data = pool_data(
            &e,
            vec![
                &e,
                tier_data_with_token(&e, first, SCALAR_7, 1),
                tier_data_with_token(&e, second.clone(), SCALAR_7, 1),
                tier_data_with_token(&e, third, SCALAR_7, 1),
            ],
        );
        let selected = select_interest_group(
            &e,
            0,
            &vec![
                &e,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC,
                0,
            ],
            &data,
        );
        assert_eq!(selected.bid_token, second);
        assert_eq!(selected.tier_indices, vec![&e, 1]);
        assert_eq!(selected.next_cursor, 2);
    }

    #[test]
    fn interest_threshold_aggregates_tiers_with_the_same_token() {
        let e = Env::default();
        let shared = Address::generate(&e);
        let distinct = Address::generate(&e);
        let data = pool_data(
            &e,
            vec![
                &e,
                tier_data_with_token(&e, shared.clone(), SCALAR_7, 1),
                tier_data_with_token(&e, distinct, SCALAR_7, 1),
                tier_data_with_token(&e, shared.clone(), SCALAR_7, 1),
            ],
        );
        let selected = select_interest_group(
            &e,
            0,
            &vec![&e, 100 * SCALAR_7, 199 * SCALAR_7, 100 * SCALAR_7],
            &data,
        );
        assert_eq!(selected.bid_token, shared);
        assert_eq!(selected.tier_indices, vec![&e, 0, 2]);
        assert_eq!(selected.next_cursor, 1);
    }

    #[test]
    fn grouped_fill_matches_public_aggregate_rounding() {
        let e = Env::default();
        let bid_token = Address::generate(&e);
        let lot_asset = Address::generate(&e);
        let auction = InterestAuctionData {
            bid_token,
            block: 1,
            tiers: vec![
                &e,
                InterestTierAuctionData {
                    bid_amount: 1,
                    lot: soroban_sdk::map![&e, (lot_asset.clone(), 1)],
                    tier: BackstopTier::FirstLoss,
                },
                InterestTierAuctionData {
                    bid_amount: 1,
                    lot: soroban_sdk::map![&e, (lot_asset.clone(), 1)],
                    tier: BackstopTier::SecondLoss,
                },
            ],
        };
        e.ledger().set_sequence_number(201);

        let fill = scale_interest_auction(&e, &auction, 50);

        assert_eq!(fill.bid_amount, 1);
        assert_eq!(fill.lot.get(lot_asset.clone()), Some(1));
        assert_eq!(fill.bids[0], 1);
        assert_eq!(fill.lots[0].get(lot_asset), Some(1));
        assert_eq!(fill.bids[1], 0);
        assert!(fill.lots[1].is_empty());
    }

    #[test]
    fn grouped_partial_fill_preserves_public_remainder_across_tier_rounding() {
        let e = Env::default();
        let contract = create_pool(&e);
        let bid_token = Address::generate(&e);
        let lot_asset = Address::generate(&e);
        let auction = InterestAuctionData {
            bid_token: bid_token.clone(),
            block: 1,
            tiers: vec![
                &e,
                InterestTierAuctionData {
                    bid_amount: 1,
                    lot: soroban_sdk::map![&e, (lot_asset.clone(), 2)],
                    tier: BackstopTier::FirstLoss,
                },
                InterestTierAuctionData {
                    bid_amount: 100,
                    lot: soroban_sdk::map![&e, (lot_asset.clone(), 200)],
                    tier: BackstopTier::SecondLoss,
                },
            ],
        };
        e.ledger().set_sequence_number(201);

        e.as_contract(&contract, || {
            let fill = scale_interest_auction(&e, &auction, 1);
            assert!(!fill.complete);
            assert_eq!(fill.bid_amount, 2);
            assert_eq!(fill.lot.get(lot_asset.clone()), Some(2));

            store_remaining_interest_auction(&e, &auction, &fill);
            let remaining = get_interest_auction(&e);
            assert_eq!(remaining.bid.get(bid_token), Some(99));
            assert_eq!(remaining.lot.get(lot_asset), Some(200));
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1228)")]
    fn interest_threshold_rejects_values_below_two_hundred_usdc() {
        let e = Env::default();
        let data = pool_data(
            &e,
            vec![
                &e,
                tier_data(&e, SCALAR_7, 1),
                tier_data(&e, SCALAR_7, 1),
                tier_data(&e, SCALAR_7, 1),
            ],
        );
        select_interest_group(
            &e,
            0,
            &vec![
                &e,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
            ],
            &data,
        );
    }

    #[test]
    fn interest_auction_singleton_is_isolated_from_bad_debt() {
        let e = Env::default();
        let contract = create_pool(&e);
        let asset = Address::generate(&e);
        let bid_token = Address::generate(&e);
        let blnt_usdc_auction = InterestAuctionData {
            bid_token: bid_token.clone(),
            block: 1,
            tiers: vec![
                &e,
                InterestTierAuctionData {
                    bid_amount: 12,
                    lot: soroban_sdk::map![&e, (asset.clone(), 10)],
                    tier: BackstopTier::SecondLoss,
                },
            ],
        };
        e.as_contract(&contract, || {
            set_interest_auction(&e, &blnt_usdc_auction);
            assert_eq!(
                get_interest_auction(&e),
                AuctionData {
                    bid: soroban_sdk::map![&e, (bid_token, 12)],
                    lot: soroban_sdk::map![&e, (asset, 10)],
                    block: 1,
                }
            );
            assert!(has_interest_auction(&e));
            assert!(
                !has_tier_auction(&e, AuctionType::BadDebtAuction),
                "interest and bad-debt auction keys must remain independent"
            );
        });
    }

    #[test]
    fn reconcile_interest_credit_scales_state_and_cancels_affected_auction() {
        let e = Env::default();
        let contract = create_pool(&e);
        let asset = Address::generate(&e);
        let bid_token = Address::generate(&e);
        e.as_contract(&contract, || {
            storage::set_protocol_fee_data(
                &e,
                &asset,
                &storage::ProtocolFeeData {
                    credit: 50,
                    carry: 7,
                },
            );
            set_interest_reserve_state(
                &e,
                &asset,
                &InterestReserveState {
                    carry: 10,
                    first_loss: 20,
                    second_loss: 30,
                    third_loss: 40,
                },
            );
            set_interest_auction(
                &e,
                &InterestAuctionData {
                    bid_token,
                    block: 1,
                    tiers: vec![
                        &e,
                        InterestTierAuctionData {
                            bid_amount: 120,
                            lot: soroban_sdk::map![&e, (asset.clone(), 100)],
                            tier: BackstopTier::FirstLoss,
                        },
                    ],
                },
            );

            assert!(reconcile_interest_credit(&e, &asset, 100, 50));
            assert!(!has_interest_auction(&e));
            assert_eq!(
                get_interest_reserve_state(&e, &asset),
                InterestReserveState {
                    carry: 5,
                    first_loss: 10,
                    second_loss: 15,
                    third_loss: 20,
                }
            );
            assert_eq!(
                storage::get_protocol_fee_data(&e, &asset),
                storage::ProtocolFeeData {
                    credit: 50,
                    carry: 7,
                }
            );
        });
    }
}
