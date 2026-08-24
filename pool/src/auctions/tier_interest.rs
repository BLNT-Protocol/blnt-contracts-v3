use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopPoolData},
    errors::PoolError,
    pool::{Pool, User},
    storage,
};
use sep_41_token::TokenClient;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Map, Vec};

use super::{
    get_tier_auction, has_tier_auction,
    math::{
        auction_modifiers, proportional_ceil, proportional_floor, scale_bid_amount,
        scale_lot_amount,
    },
    remove_tier_auction, require_unique_addresses, set_tier_auction, to_backstop_tier, AuctionData,
    AuctionType, TierAuctionData,
};

const ONE_DAY_LEDGERS: u32 = 17_280;
const MAX_INTEREST_LOT_ASSETS: u32 = 4;
const INTEREST_AUCTION_MINIMUM_VALUE_USDC: i128 = 200 * SCALAR_7;
const INTEREST_STATE_TTL_THRESHOLD: u32 = 179 * ONE_DAY_LEDGERS;
const INTEREST_STATE_TTL_BUMP: u32 = 180 * ONE_DAY_LEDGERS;

pub(crate) fn create_interest_auction_data(e: &Env, lot_assets: &Vec<Address>) -> TierAuctionData {
    if has_tier_auction(e, AuctionType::InterestAuction) {
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

    let tier = select_interest_tier(e, interest_tier_cursor(e), &tier_values);
    let lot_value = tier_values.get(tier_index(tier)).unwrap();
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
        set_interest_reserve_state(e, &asset, &state);
    }
    if lot.is_empty() {
        panic_with_error!(e, PoolError::NoInterestAuctionCapacity);
    }

    let (bid_token, bid_amount) = build_interest_bid(e, &backstop, &pool_data, tier, lot_value);
    let block = e
        .ledger()
        .sequence()
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let auction = TierAuctionData {
        auction: AuctionData {
            bid: soroban_sdk::map![e, (bid_token, bid_amount)],
            lot,
            block,
        },
        tier,
    };
    set_tier_auction(e, AuctionType::InterestAuction, &auction);
    e.storage().instance().set(
        &InterestDataKey::Cursor,
        &((tier_index(tier) + 1) % pool_data.tiers.len()),
    );
    pool.store_cached_reserves(e);
    auction
}

pub(crate) fn get_interest_auction(e: &Env) -> TierAuctionData {
    get_tier_auction(e, AuctionType::InterestAuction)
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

    let auction = get_interest_auction(e);
    let fill = scale_interest_auction(e, &auction, percent);
    for (asset, amount) in fill.lot.iter() {
        let mut reserve = pool.load_reserve(e, &asset, true);
        reserve.require_authorized(e);
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
    consume_pending_interest_lot(e, auction.tier, &fill.lot);

    if fill.bid_amount > 0 {
        BackstopClient::new(e, &backstop).donate(
            &to_backstop_tier(auction.tier),
            &filler_state.address,
            &pool_address,
            &fill.bid_amount,
        );
    }
    if fill.complete {
        remove_tier_auction(e, AuctionType::InterestAuction);
    } else {
        store_remaining_interest_auction(e, &auction, &fill);
    }
    let bid_token = auction.auction.bid.keys().get(0).unwrap();
    let bid = if fill.bid_amount > 0 {
        soroban_sdk::map![e, (bid_token, fill.bid_amount)]
    } else {
        soroban_sdk::map![e]
    };
    AuctionData {
        bid,
        lot: fill.lot,
        block: fill.block,
    }
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

/// Exact base and time-scaled amounts processed by one interest fill.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub(crate) struct InterestAuctionFill {
    pub base_bid_amount: i128,
    pub base_lot: Map<Address, i128>,
    pub bid_amount: i128,
    pub block: u32,
    pub complete: bool,
    pub lot: Map<Address, i128>,
    pub tier: super::BackstopTier,
}

#[derive(Clone)]
#[contracttype]
enum InterestDataKey {
    Cursor,
    Reserve(Address),
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

    let auction_canceled = if has_tier_auction(e, AuctionType::InterestAuction) {
        let auction = get_tier_auction(e, AuctionType::InterestAuction);
        if auction.auction.lot.contains_key(asset.clone()) {
            remove_tier_auction(e, AuctionType::InterestAuction);
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

fn build_interest_bid(
    e: &Env,
    backstop: &Address,
    pool_data: &BackstopPoolData,
    tier: super::BackstopTier,
    lot_value: i128,
) -> (Address, i128) {
    let tier_data = pool_data
        .tiers
        .get(tier_index(tier))
        .unwrap_or_else(|| panic_with_error!(e, PoolError::InvalidLot));
    let tokens = tier_data.tokens;
    let shares = tier_data.shares;
    let value = tier_data.value;
    if lot_value <= 0 || tokens <= 0 || shares <= 0 || value <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }

    let bid_token = BackstopClient::new(e, backstop)
        .backstop_token(&to_backstop_tier(tier), &e.current_contract_address());
    if TokenClient::new(e, &bid_token).decimals() != 7 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let target_value = proportional_ceil(e, lot_value, 6, 5);
    let bid_amount = proportional_ceil(e, target_value, tokens, value);
    if bid_amount <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    (bid_token, bid_amount)
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

fn store_remaining_interest_auction(
    e: &Env,
    auction: &TierAuctionData,
    fill: &InterestAuctionFill,
) {
    let bid_token = auction.auction.bid.keys().get(0).unwrap();
    let previous_bid = auction.auction.bid.get(bid_token.clone()).unwrap();
    let remaining_bid = checked_sub(e, previous_bid, fill.base_bid_amount);
    if remaining_bid <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    let mut remaining_lot = Map::new(e);
    for (asset, amount) in auction.auction.lot.iter() {
        let remainder = checked_sub(e, amount, fill.base_lot.get(asset.clone()).unwrap_or(0));
        if remainder > 0 {
            remaining_lot.set(asset, remainder);
        }
    }
    set_tier_auction(
        e,
        AuctionType::InterestAuction,
        &TierAuctionData {
            auction: AuctionData {
                bid: soroban_sdk::map![e, (bid_token, remaining_bid)],
                lot: remaining_lot,
                block: auction.auction.block,
            },
            tier: auction.tier,
        },
    );
}

fn scale_interest_auction(e: &Env, auction: &TierAuctionData, percent: u32) -> InterestAuctionFill {
    if percent == 0 || percent > 100 || auction.auction.bid.len() != 1 {
        panic_with_error!(e, PoolError::InvalidInterestAuction);
    }
    let elapsed = e
        .ledger()
        .sequence()
        .checked_sub(auction.auction.block)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::InvalidInterestAuction));
    let (bid_modifier, lot_modifier) = auction_modifiers(e, elapsed);
    let percent_scaled = i128::from(percent)
        .checked_mul(100_000)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let (_, bid_amount) = auction.auction.bid.iter().next().unwrap();
    let (base_bid_amount, actual_bid_amount, remaining_bid) =
        scale_bid_amount(e, bid_amount, percent_scaled, bid_modifier);
    let complete = remaining_bid == 0;
    let mut base_lot = Map::new(e);
    let mut lot = Map::new(e);
    for (asset, amount) in auction.auction.lot.iter() {
        let (base, actual, _) = scale_lot_amount(e, amount, percent_scaled, lot_modifier);
        if base > 0 {
            base_lot.set(asset.clone(), base);
        }
        if actual > 0 {
            lot.set(asset, actual);
        }
    }
    InterestAuctionFill {
        base_bid_amount,
        base_lot,
        bid_amount: actual_bid_amount,
        block: auction.auction.block,
        complete,
        lot,
        tier: auction.tier,
    }
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

fn select_interest_tier(e: &Env, cursor: u32, tier_values: &Vec<i128>) -> super::BackstopTier {
    if tier_values.is_empty() || cursor >= tier_values.len() {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    for offset in 0..tier_values.len() {
        let index = (cursor + offset) % tier_values.len();
        if tier_values.get(index).unwrap() >= INTEREST_AUCTION_MINIMUM_VALUE_USDC {
            return tier_from_index(e, index);
        }
    }
    panic_with_error!(e, PoolError::NoInterestAuctionCapacity)
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
    use soroban_sdk::{testutils::Address as _, vec};

    use crate::testutils::create_pool;

    use super::*;

    fn tier_data(e: &Env, value: i128, weight: u32) -> crate::dependencies::BackstopPoolTierData {
        crate::dependencies::BackstopPoolTierData {
            asset: crate::dependencies::BackstopContractAsset::BlndXlm,
            blnd_emission_eligible: false,
            take_rate_weight: weight,
            token: Address::generate(e),
            tokens: value,
            shares: value,
            value,
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
        assert_eq!(
            select_interest_tier(
                &e,
                0,
                &vec![
                    &e,
                    INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                    INTEREST_AUCTION_MINIMUM_VALUE_USDC,
                    0,
                ],
            ),
            super::super::BackstopTier::SecondLoss
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1228)")]
    fn interest_threshold_rejects_values_below_two_hundred_usdc() {
        let e = Env::default();
        select_interest_tier(
            &e,
            0,
            &vec![
                &e,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
            ],
        );
    }

    #[test]
    fn interest_auction_singleton_is_isolated_from_bad_debt() {
        let e = Env::default();
        let contract = create_pool(&e);
        let asset = Address::generate(&e);
        let bid_token = Address::generate(&e);
        let blnd_usdc_auction = TierAuctionData {
            auction: AuctionData {
                bid: soroban_sdk::map![&e, (bid_token, 12)],
                lot: soroban_sdk::map![&e, (asset, 10)],
                block: 1,
            },
            tier: super::super::BackstopTier::SecondLoss,
        };
        e.as_contract(&contract, || {
            set_tier_auction(&e, AuctionType::InterestAuction, &blnd_usdc_auction);
            assert_eq!(get_interest_auction(&e), blnd_usdc_auction);
            assert!(has_tier_auction(&e, AuctionType::InterestAuction));
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
            set_tier_auction(
                &e,
                AuctionType::InterestAuction,
                &TierAuctionData {
                    auction: AuctionData {
                        bid: soroban_sdk::map![&e, (bid_token, 120)],
                        lot: soroban_sdk::map![&e, (asset.clone(), 100)],
                        block: 1,
                    },
                    tier: super::super::BackstopTier::FirstLoss,
                },
            );

            assert!(reconcile_interest_credit(&e, &asset, 100, 50));
            assert!(!has_tier_auction(&e, AuctionType::InterestAuction));
            assert_eq!(
                get_interest_reserve_state(&e, &asset),
                InterestReserveState {
                    carry: 5,
                    first_loss: 10,
                    second_loss: 15,
                    third_loss: 20,
                }
            );
        });
    }
}
