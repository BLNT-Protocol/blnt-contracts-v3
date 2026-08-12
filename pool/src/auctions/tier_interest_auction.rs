use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopContractTier, BackstopPoolData},
    errors::PoolError,
    pool::{Pool, User},
    storage,
};
use sep_41_token::TokenClient;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Map, Vec};

use super::{
    math::{auction_modifiers, proportional_ceil, proportional_floor},
    AuctionData,
};

const ONE_DAY_LEDGERS: u32 = 17_280;
const AUCTION_TTL_THRESHOLD: u32 = 45 * ONE_DAY_LEDGERS;
const AUCTION_TTL_BUMP: u32 = 46 * ONE_DAY_LEDGERS;
const AUCTION_STALE_LEDGERS: u32 = 500;
const MAX_INTEREST_LOT_ASSETS: u32 = 4;
const INTEREST_AUCTION_MINIMUM_VALUE_USDC: i128 = 200 * SCALAR_7;
const INTEREST_STATE_TTL_THRESHOLD: u32 = 179 * ONE_DAY_LEDGERS;
const INTEREST_STATE_TTL_BUMP: u32 = 180 * ONE_DAY_LEDGERS;
const TAKE_RATE_WEIGHT_BLND_XLM: i128 = 4;
const TAKE_RATE_WEIGHT_BLND_USDC: i128 = 3;
const TAKE_RATE_WEIGHT_USDC: i128 = 2;

/// Pending reserve credit apportioned to each tier for one reserve asset.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
struct InterestReserveState {
    blnd_usdc: i128,
    blnd_xlm: i128,
    carry: i128,
    usdc: i128,
}

impl InterestReserveState {
    fn empty() -> Self {
        Self {
            blnd_usdc: 0,
            blnd_xlm: 0,
            carry: 0,
            usdc: 0,
        }
    }

    fn total(&self, e: &Env) -> i128 {
        checked_add(
            e,
            checked_add(e, self.blnd_usdc, self.blnd_xlm),
            checked_add(e, self.usdc, self.carry),
        )
    }

    fn tier_amount(&self, tier: super::BackstopTier) -> i128 {
        match tier {
            super::BackstopTier::BlndUsdc => self.blnd_usdc,
            super::BackstopTier::BlndXlm => self.blnd_xlm,
            super::BackstopTier::Usdc => self.usdc,
        }
    }

    fn set_tier_amount(&mut self, tier: super::BackstopTier, amount: i128) {
        match tier {
            super::BackstopTier::BlndUsdc => self.blnd_usdc = amount,
            super::BackstopTier::BlndXlm => self.blnd_xlm = amount,
            super::BackstopTier::Usdc => self.usdc = amount,
        }
    }
}

/// One active tier-specific interest auction.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub struct InterestAuctionData {
    pub auction: AuctionData,
    pub tier: super::BackstopTier,
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
enum InterestAuctionDataKey {
    InterestAuction,
}

#[derive(Clone)]
#[contracttype]
enum InterestDataKey {
    Cursor,
    Reserve(Address),
}

pub fn create_interest_auction(e: &Env, lot_assets: &Vec<Address>) -> InterestAuctionData {
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
    require_unique_assets(e, lot_assets);

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
    let take_rate_values = [
        pool_data.blnd_usdc.value,
        pool_data.blnd_xlm.value,
        pool_data.usdc.value,
    ];
    for (asset, distribution) in distributions.iter() {
        let allocation = allocate_take_rate(e, distribution, take_rate_values);
        apply_take_rate_allocation(e, &mut states, &asset, allocation);
    }

    let mut tier_values = [0_i128; 3];
    for asset in lot_assets {
        let state = states.get(asset.clone()).unwrap();
        let reserve = pool.load_reserve(e, &asset, false);
        let price = pool.load_price(e, &asset);
        let values = [
            value_reserve_amount(e, price, state.blnd_usdc, reserve.scalar, oracle_scalar),
            value_reserve_amount(e, price, state.blnd_xlm, reserve.scalar, oracle_scalar),
            value_reserve_amount(e, price, state.usdc, reserve.scalar, oracle_scalar),
        ];
        for index in 0..3 {
            tier_values[index] = checked_add(e, tier_values[index], values[index]);
        }
    }

    let tier = select_interest_tier(e, interest_tier_cursor(e), tier_values);
    let lot_value = tier_values[tier_index(tier) as usize];
    let mut lot = Map::new(e);
    for asset in lot_assets {
        let state = states.get(asset.clone()).unwrap();
        let amount = state.tier_amount(tier);
        if amount > 0 {
            lot.set(asset.clone(), amount);
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
    let auction = InterestAuctionData {
        auction: AuctionData {
            bid: soroban_sdk::map![e, (bid_token, bid_amount)],
            lot,
            block,
        },
        tier,
    };
    set_interest_auction(e, &auction);
    e.storage()
        .instance()
        .set(&InterestDataKey::Cursor, &((tier_index(tier) + 1) % 3));
    pool.store_cached_reserves(e);
    auction
}

pub fn get_interest_auction(e: &Env) -> InterestAuctionData {
    e.storage()
        .temporary()
        .get(&InterestAuctionDataKey::InterestAuction)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest))
}

pub fn fill_interest_auction(
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
        let token = TokenClient::new(e, &asset);
        let pool_before = token.balance(&pool_address);
        let filler_before = token.balance(&filler_state.address);
        token.transfer(&pool_address, &filler_state.address, &amount);
        if token.balance(&pool_address) != checked_sub(e, pool_before, amount)
            || token.balance(&filler_state.address) != checked_add(e, filler_before, amount)
        {
            panic_with_error!(e, PoolError::BalanceError);
        }
        let mut reserve = pool.load_reserve(e, &asset, true);
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
        e.storage()
            .temporary()
            .remove(&InterestAuctionDataKey::InterestAuction);
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

pub fn del_interest_auction(e: &Env) {
    let auction = get_interest_auction(e);
    let stale_at = auction
        .auction
        .block
        .checked_add(AUCTION_STALE_LEDGERS)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if e.ledger().sequence() < stale_at {
        panic_with_error!(e, PoolError::BadRequest);
    }
    e.storage()
        .temporary()
        .remove(&InterestAuctionDataKey::InterestAuction);
}

fn has_interest_auction(e: &Env) -> bool {
    e.storage()
        .temporary()
        .has(&InterestAuctionDataKey::InterestAuction)
}

fn set_interest_auction(e: &Env, auction: &InterestAuctionData) {
    let key = InterestAuctionDataKey::InterestAuction;
    e.storage().temporary().set(&key, auction);
    e.storage()
        .temporary()
        .extend_ttl(&key, AUCTION_TTL_THRESHOLD, AUCTION_TTL_BUMP);
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
    if state.blnd_usdc < 0 || state.blnd_xlm < 0 || state.usdc < 0 || state.carry < 0 {
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

fn allocate_take_rate(e: &Env, distribution: i128, values: [i128; 3]) -> InterestReserveState {
    if distribution < 0 || values.iter().any(|value| *value < 0) {
        panic_with_error!(e, PoolError::InvalidLot);
    }

    let blnd_usdc_weighted = checked_mul(e, values[0], TAKE_RATE_WEIGHT_BLND_USDC);
    let blnd_xlm_weighted = checked_mul(e, values[1], TAKE_RATE_WEIGHT_BLND_XLM);
    let usdc_weighted = checked_mul(e, values[2], TAKE_RATE_WEIGHT_USDC);
    let denominator = checked_add(
        e,
        checked_add(e, blnd_usdc_weighted, blnd_xlm_weighted),
        usdc_weighted,
    );
    if denominator == 0 {
        return InterestReserveState {
            blnd_usdc: 0,
            blnd_xlm: 0,
            carry: distribution,
            usdc: 0,
        };
    }

    let blnd_usdc = proportional_floor(e, distribution, blnd_usdc_weighted, denominator);
    let blnd_xlm = proportional_floor(e, distribution, blnd_xlm_weighted, denominator);
    let usdc = proportional_floor(e, distribution, usdc_weighted, denominator);
    let allocated = checked_add(e, checked_add(e, blnd_usdc, blnd_xlm), usdc);
    InterestReserveState {
        blnd_usdc,
        blnd_xlm,
        carry: checked_sub(e, distribution, allocated),
        usdc,
    }
}

fn apply_take_rate_allocation(
    e: &Env,
    states: &mut Map<Address, InterestReserveState>,
    asset: &Address,
    allocation: InterestReserveState,
) {
    let mut state = states.get(asset.clone()).unwrap();
    state.blnd_usdc = checked_add(e, state.blnd_usdc, allocation.blnd_usdc);
    state.blnd_xlm = checked_add(e, state.blnd_xlm, allocation.blnd_xlm);
    state.usdc = checked_add(e, state.usdc, allocation.usdc);
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
    let (tokens, shares, value) = match tier {
        super::BackstopTier::BlndUsdc => (
            pool_data.blnd_usdc.tokens,
            pool_data.blnd_usdc.shares,
            pool_data.blnd_usdc.value,
        ),
        super::BackstopTier::BlndXlm => (
            pool_data.blnd_xlm.tokens,
            pool_data.blnd_xlm.shares,
            pool_data.blnd_xlm.value,
        ),
        super::BackstopTier::Usdc => (
            pool_data.usdc.tokens,
            pool_data.usdc.shares,
            pool_data.usdc.value,
        ),
    };
    if lot_value <= 0 || tokens <= 0 || shares <= 0 || value <= 0 {
        panic_with_error!(e, PoolError::InvalidLot);
    }

    let bid_token = BackstopClient::new(e, backstop).backstop_token(&to_backstop_tier(tier));
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
    auction: &InterestAuctionData,
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
    set_interest_auction(
        e,
        &InterestAuctionData {
            auction: AuctionData {
                bid: soroban_sdk::map![e, (bid_token, remaining_bid)],
                lot: remaining_lot,
                block: auction.auction.block,
            },
            tier: auction.tier,
        },
    );
}

fn scale_interest_auction(
    e: &Env,
    auction: &InterestAuctionData,
    percent: u32,
) -> InterestAuctionFill {
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
    let base_bid_amount = bid_amount.fixed_mul_ceil(e, &percent_scaled, &SCALAR_7);
    let remaining_bid = checked_sub(e, bid_amount, base_bid_amount);
    let actual_bid_amount = base_bid_amount.fixed_mul_ceil(e, &bid_modifier, &SCALAR_7);
    let complete = remaining_bid == 0;
    let mut base_lot = Map::new(e);
    let mut lot = Map::new(e);
    for (asset, amount) in auction.auction.lot.iter() {
        let base = amount.fixed_mul_floor(e, &percent_scaled, &SCALAR_7);
        let actual = base.fixed_mul_floor(e, &lot_modifier, &SCALAR_7);
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

fn tier_from_index(index: u32) -> super::BackstopTier {
    match index {
        0 => super::BackstopTier::BlndUsdc,
        1 => super::BackstopTier::BlndXlm,
        _ => super::BackstopTier::Usdc,
    }
}

fn select_interest_tier(e: &Env, cursor: u32, tier_values: [i128; 3]) -> super::BackstopTier {
    for offset in 0..3_u32 {
        let index = (cursor + offset) % 3;
        if tier_values[index as usize] >= INTEREST_AUCTION_MINIMUM_VALUE_USDC {
            return tier_from_index(index);
        }
    }
    panic_with_error!(e, PoolError::NoInterestAuctionCapacity)
}

fn tier_index(tier: super::BackstopTier) -> u32 {
    match tier {
        super::BackstopTier::BlndUsdc => 0,
        super::BackstopTier::BlndXlm => 1,
        super::BackstopTier::Usdc => 2,
    }
}

fn to_backstop_tier(tier: super::BackstopTier) -> BackstopContractTier {
    match tier {
        super::BackstopTier::BlndUsdc => BackstopContractTier::BlndUsdc,
        super::BackstopTier::BlndXlm => BackstopContractTier::BlndXlm,
        super::BackstopTier::Usdc => BackstopContractTier::Usdc,
    }
}

fn require_unique_assets(e: &Env, assets: &Vec<Address>) {
    let mut seen = Map::<Address, bool>::new(e);
    for asset in assets {
        if seen.contains_key(asset.clone()) {
            panic_with_error!(e, PoolError::BadRequest);
        }
        seen.set(asset, true);
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
    use soroban_sdk::testutils::Address as _;

    use crate::testutils::create_pool;

    use super::*;

    #[test]
    fn take_rate_allocation_applies_pool_weights() {
        let e = Env::default();
        let allocation = allocate_take_rate(&e, 90, [1, 1, 1]);

        assert_eq!(
            allocation,
            InterestReserveState {
                blnd_usdc: 30,
                blnd_xlm: 40,
                carry: 0,
                usdc: 20,
            }
        );
    }

    #[test]
    fn take_rate_allocation_conserves_rounding_remainder() {
        let e = Env::default();
        let allocation = allocate_take_rate(&e, 10, [3, 2, 1]);

        assert_eq!(
            allocation.blnd_usdc + allocation.blnd_xlm + allocation.usdc + allocation.carry,
            10
        );
        assert_eq!(allocation.carry, 1);
    }

    #[test]
    fn interest_threshold_includes_exactly_two_hundred_usdc() {
        let e = Env::default();
        assert_eq!(
            select_interest_tier(
                &e,
                0,
                [
                    INTEREST_AUCTION_MINIMUM_VALUE_USDC - 1,
                    INTEREST_AUCTION_MINIMUM_VALUE_USDC,
                    0,
                ],
            ),
            super::super::BackstopTier::BlndXlm
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1228)")]
    fn interest_threshold_rejects_values_below_two_hundred_usdc() {
        let e = Env::default();
        select_interest_tier(
            &e,
            0,
            [
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
        let blnd_usdc_auction = InterestAuctionData {
            auction: AuctionData {
                bid: soroban_sdk::map![&e, (bid_token, 12)],
                lot: soroban_sdk::map![&e, (asset, 10)],
                block: 1,
            },
            tier: super::super::BackstopTier::BlndUsdc,
        };
        e.as_contract(&contract, || {
            set_interest_auction(&e, &blnd_usdc_auction);
            assert_eq!(get_interest_auction(&e), blnd_usdc_auction);
            assert!(has_interest_auction(&e));
            assert!(
                !super::super::bad_debt_auction::has_prepared_bad_debt_auction(&e),
                "interest and bad-debt auction keys must remain independent"
            );
        });
    }
}
