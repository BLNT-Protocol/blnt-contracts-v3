use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopContractBadDebtLotQuote, BackstopContractTier},
    errors::PoolError,
    pool::{
        backstop_liabilities, backstop_liability, check_and_handle_backstop_bad_debt,
        sync_backstop_liabilities, Pool, PositionData, User,
    },
    storage,
};
use cast::i128;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{contracttype, map, panic_with_error, Address, BytesN, Env, Map, Vec};

use super::{AuctionData, AuctionType};

const ONE_DAY_LEDGERS: u32 = 17_280;
const AUCTION_TTL_THRESHOLD: u32 = 45 * ONE_DAY_LEDGERS;
const AUCTION_TTL_BUMP: u32 = 46 * ONE_DAY_LEDGERS;
const AUCTION_STALE_LEDGERS: u32 = 500;
/// Preserve footprint headroom for stateful per-reserve oracle reads while a
/// continuation also validates the complete bounded reserve set.
const MAX_BAD_DEBT_BID_ASSETS: u32 = 4;

/// The fixed v3 backstop tier identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopTier {
    BlndUsdc,
    BlndXlm,
    Usdc,
}

/// Canonical single-tier lot returned by the configured backstop.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BadDebtLotQuote {
    pub committed_value: i128,
    pub debt_value: i128,
    pub tier: BackstopTier,
    pub lot_amount: i128,
    pub unfilled_target_value: i128,
    pub target_value: i128,
    pub valid_until: u64,
}

/// Prepared or partially filled single-tier bad-debt auction data.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BadDebtAuctionData {
    pub auction_id: BytesN<32>,
    pub bid: Map<Address, i128>,
    pub block: u32,
    pub lot_quote: BadDebtLotQuote,
}

/// Exact amounts processed by one bad-debt auction fill.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BadDebtAuctionFill {
    pub auction_id: BytesN<32>,
    /// Base lot removed from the commitment before the time modifier.
    pub base_lot_amount: i128,
    /// Time-scaled dToken shares assumed by the filler.
    pub bid: Map<Address, i128>,
    pub block: u32,
    pub complete: bool,
    /// Time-scaled tier-token amount transferred to the filler.
    pub lot_amount: i128,
    pub tier: BackstopTier,
}

/// Result of one bounded permissionless bad-debt continuation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BadDebtContinuation {
    /// True when the supplied ID identifies the next active auction.
    pub auction_created: bool,
    /// dToken shares defaulted to suppliers, keyed by reserve asset.
    pub defaulted: Map<Address, i128>,
}

#[derive(Clone)]
#[contracttype]
enum PreparedBadDebtDataKey {
    Auction,
}

/// Prepare a pool-authorized bad-debt lot without moving positions or assets.
pub fn create_prepared_bad_debt_auction(
    e: &Env,
    auction_id: &BytesN<32>,
    bid: &Vec<Address>,
) -> BadDebtAuctionData {
    let backstop = storage::get_backstop(e);
    if has_prepared_bad_debt_auction(e)
        || storage::has_auction(e, &(AuctionType::BadDebtAuction as u32), &backstop)
    {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let mut pool = Pool::load(e);
    if pool.config.max_positions <= bid.len() {
        panic_with_error!(e, PoolError::MaxPositionsExceeded);
    }
    require_unique_bid_assets(e, bid);

    let (bid_amounts, debt_value_usdc) = build_bad_debt_bid(e, &mut pool, bid);
    commit_prepared_bad_debt_auction(e, auction_id, &bid_amounts, debt_value_usdc, None)
}

/// Continue the strict single-tier waterfall without caller authorization.
pub fn continue_bad_debt_resolution(e: &Env, auction_id: &BytesN<32>) -> BadDebtContinuation {
    let backstop = storage::get_backstop(e);
    if has_prepared_bad_debt_auction(e)
        || storage::has_auction(e, &(AuctionType::BadDebtAuction as u32), &backstop)
    {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let mut pool = Pool::load(e);
    let bid = canonical_bad_debt_bid(e, &pool);
    let (bid_amounts, debt_value_usdc) = build_bad_debt_bid(e, &mut pool, &bid);
    let pool_address = e.current_contract_address();
    let quoted =
        BackstopClient::new(e, &backstop).quote_bad_debt_lot(&pool_address, &debt_value_usdc);

    if let Some(quote) = quoted {
        let expected_quote = convert_backstop_quote(quote);
        commit_prepared_bad_debt_auction(
            e,
            auction_id,
            &bid_amounts,
            debt_value_usdc,
            Some(&expected_quote),
        );
        BadDebtContinuation {
            auction_created: true,
            defaulted: Map::new(e),
        }
    } else {
        BadDebtContinuation {
            auction_created: false,
            defaulted: default_all_backstop_liabilities(e, pool),
        }
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
        if liability_balance <= 0 || backstop_liability(e, &reserve.asset) != liability_balance {
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
    // continuation ahead of later liabilities or supplier settlement.
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
    auction_id: &BytesN<32>,
    bid_amounts: &Map<Address, i128>,
    debt_value_usdc: i128,
    expected_quote: Option<&BadDebtLotQuote>,
) -> BadDebtAuctionData {
    let backstop = storage::get_backstop(e);
    let pool_address = e.current_contract_address();
    let backstop_quote = BackstopClient::new(e, &backstop).commit_bad_debt_lot(
        &pool_address,
        auction_id,
        &debt_value_usdc,
    );
    let lot_quote = convert_backstop_quote(backstop_quote);
    if lot_quote.debt_value != debt_value_usdc
        || lot_quote.committed_value <= 0
        || lot_quote.lot_amount <= 0
        || lot_quote.valid_until < e.ledger().timestamp()
    {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    if expected_quote.is_some_and(|quote| quote != &lot_quote) {
        panic_with_error!(e, PoolError::InvalidLot);
    }

    let block = e
        .ledger()
        .sequence()
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let auction = BadDebtAuctionData {
        auction_id: auction_id.clone(),
        bid: bid_amounts.clone(),
        block,
        lot_quote,
    };
    set_prepared_bad_debt_auction(e, &auction);
    auction
}

fn canonical_bad_debt_bid(e: &Env, pool: &Pool) -> Vec<Address> {
    let backstop = storage::get_backstop(e);
    let backstop_positions = storage::get_user_positions(e, &backstop);
    let recorded = backstop_liabilities(e);
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
        if amount < 0 || backstop_liability(e, &asset) != amount {
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
    if liability_count == 0
        || recorded.len() != liability_count
        || backstop_positions.liabilities.len() != liability_count
    {
        panic_with_error!(e, PoolError::InvalidBid);
    }
    bid
}

fn default_all_backstop_liabilities(e: &Env, mut pool: Pool) -> Map<Address, i128> {
    let backstop = storage::get_backstop(e);
    let mut backstop_state = User::load(e, &backstop);
    let mut defaulted = Map::new(e);

    for asset in storage::get_res_list(e) {
        let reserve_config = storage::get_res_config(e, &asset);
        let amount = backstop_state.get_liabilities(reserve_config.index);
        if amount == 0 {
            continue;
        }
        let mut reserve = pool.load_reserve(e, &asset, true);
        backstop_state.default_liabilities(e, &mut reserve, amount);
        pool.cache_reserve(reserve);
        defaulted.set(asset.clone(), amount);
        crate::events::PoolEvents::defaulted_debt(e, asset, amount);
    }

    if backstop_state.has_liabilities() || defaulted.is_empty() {
        panic_with_error!(e, PoolError::InvalidBid);
    }
    sync_backstop_liabilities(e, &backstop_state.positions);
    backstop_state.store(e);
    pool.store_cached_reserves(e);
    defaulted
}

pub fn get_prepared_bad_debt_auction(e: &Env) -> BadDebtAuctionData {
    e.storage()
        .temporary()
        .get(&PreparedBadDebtDataKey::Auction)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest))
}

/// Atomically transfer the scaled debt bid and settle the realized tier loss.
pub fn fill_prepared_bad_debt_auction(
    e: &Env,
    filler: &Address,
    percent: u32,
) -> BadDebtAuctionFill {
    let pool_address = e.current_contract_address();
    let backstop = storage::get_backstop(e);
    if filler == &pool_address || filler == &backstop || percent == 0 || percent > 100 {
        panic_with_error!(e, PoolError::InvalidLiquidation);
    }
    filler.require_auth();
    if storage::has_auction(e, &(AuctionType::UserLiquidation as u32), filler) {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let auction = get_prepared_bad_debt_auction(e);
    let (fill, remaining_bid, remaining_lot_amount) =
        scale_prepared_bad_debt_auction(e, &auction, percent);
    let mut pool = Pool::load(e);
    let mut backstop_state = User::load(e, &backstop);
    let mut filler_state = User::load(e, filler);
    let filler_previous_count = filler_state.positions.effective_count();

    for (asset, _) in auction.bid.iter() {
        let reserve = pool.load_reserve(e, &asset, false);
        if backstop_liability(e, &asset) != backstop_state.get_liabilities(reserve.config.index) {
            panic_with_error!(e, PoolError::InvalidBid);
        }
    }

    backstop_state.rm_positions(e, &mut pool, map![e], fill.bid.clone());
    filler_state.add_positions(e, &mut pool, map![e], fill.bid.clone());
    pool.require_under_max(e, &filler_state.positions, filler_previous_count);
    if filler_state.has_liabilities() {
        let position_data =
            PositionData::calculate_from_positions(e, &mut pool, &filler_state.positions);
        if position_data.is_hf_under(e, 1_0000100) {
            panic_with_error!(e, PoolError::InvalidHf);
        }
        if position_data.collateral_base < pool.config.min_collateral {
            panic_with_error!(e, PoolError::MinCollateralNotMet);
        }
    }

    let backstop_remaining = BackstopClient::new(e, &backstop).settle_bad_debt_lot(
        &pool_address,
        &auction.auction_id,
        &fill.base_lot_amount,
        &fill.lot_amount,
        filler,
    );
    let remaining_quote = backstop_remaining.map(convert_backstop_quote);
    if fill.complete != remaining_quote.is_none() {
        panic_with_error!(e, PoolError::InvalidLot);
    }
    if let Some(remaining_quote) = remaining_quote {
        let expected_committed_value = auction.lot_quote.committed_value.fixed_mul_floor(
            e,
            &remaining_lot_amount,
            &auction.lot_quote.lot_amount,
        );
        if remaining_quote.lot_amount != remaining_lot_amount
            || remaining_quote.committed_value != expected_committed_value
            || remaining_quote.debt_value != auction.lot_quote.debt_value
            || remaining_quote.tier != auction.lot_quote.tier
            || remaining_quote.target_value != auction.lot_quote.target_value
            || remaining_quote.unfilled_target_value != auction.lot_quote.unfilled_target_value
            || remaining_quote.valid_until != auction.lot_quote.valid_until
        {
            panic_with_error!(e, PoolError::InvalidLot);
        }
        set_prepared_bad_debt_auction(
            e,
            &BadDebtAuctionData {
                auction_id: auction.auction_id,
                bid: remaining_bid,
                block: auction.block,
                lot_quote: remaining_quote,
            },
        );
    } else {
        e.storage()
            .temporary()
            .remove(&PreparedBadDebtDataKey::Auction);
    }

    sync_backstop_liabilities(e, &backstop_state.positions);
    backstop_state.store(e);
    filler_state.store(e);
    pool.store_cached_reserves(e);
    fill
}

pub fn delete_stale_prepared_bad_debt_auction(e: &Env) -> BytesN<32> {
    let auction = get_prepared_bad_debt_auction(e);
    let stale_at = auction
        .block
        .checked_add(AUCTION_STALE_LEDGERS)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if e.ledger().sequence() < stale_at {
        panic_with_error!(e, PoolError::BadRequest);
    }

    BackstopClient::new(e, &storage::get_backstop(e))
        .release_bad_debt_lot(&e.current_contract_address(), &auction.auction_id);
    e.storage()
        .temporary()
        .remove(&PreparedBadDebtDataKey::Auction);
    auction.auction_id
}

pub fn has_prepared_bad_debt_auction(e: &Env) -> bool {
    e.storage()
        .temporary()
        .has(&PreparedBadDebtDataKey::Auction)
}

fn set_prepared_bad_debt_auction(e: &Env, auction: &BadDebtAuctionData) {
    e.storage()
        .temporary()
        .set(&PreparedBadDebtDataKey::Auction, auction);
    e.storage().temporary().extend_ttl(
        &PreparedBadDebtDataKey::Auction,
        AUCTION_TTL_THRESHOLD,
        AUCTION_TTL_BUMP,
    );
}

fn scale_prepared_bad_debt_auction(
    e: &Env,
    auction: &BadDebtAuctionData,
    percent: u32,
) -> (BadDebtAuctionFill, Map<Address, i128>, i128) {
    if percent == 0 || percent > 100 {
        panic_with_error!(e, PoolError::InvalidLiquidation);
    }
    let (bid_modifier, lot_modifier) = auction_modifiers(e, auction.block);
    let percent_scaled = i128::from(percent)
        .checked_mul(100_000)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let mut filled_bid = Map::new(e);
    let mut remaining_bid = Map::new(e);
    for (asset, amount) in auction.bid.iter() {
        let base = amount.fixed_mul_ceil(e, &percent_scaled, &SCALAR_7);
        let remainder = amount
            .checked_sub(base)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        if remainder > 0 {
            remaining_bid.set(asset.clone(), remainder);
        }
        let scaled = base.fixed_mul_ceil(e, &bid_modifier, &SCALAR_7);
        if scaled > 0 {
            filled_bid.set(asset, scaled);
        }
    }
    let base_lot_amount =
        auction
            .lot_quote
            .lot_amount
            .fixed_mul_floor(e, &percent_scaled, &SCALAR_7);
    let remaining_lot_amount = auction
        .lot_quote
        .lot_amount
        .checked_sub(base_lot_amount)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let lot_amount = base_lot_amount.fixed_mul_floor(e, &lot_modifier, &SCALAR_7);
    let complete = remaining_bid.is_empty() && remaining_lot_amount == 0;
    (
        BadDebtAuctionFill {
            auction_id: auction.auction_id.clone(),
            base_lot_amount,
            bid: filled_bid,
            block: auction.block,
            complete,
            lot_amount,
            tier: auction.lot_quote.tier,
        },
        remaining_bid,
        remaining_lot_amount,
    )
}

#[allow(clippy::zero_prefixed_literal)]
fn auction_modifiers(e: &Env, block: u32) -> (i128, i128) {
    let current_ledger = e.ledger().sequence();
    let elapsed = current_ledger
        .checked_sub(block)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest));
    let per_ledger = 0_0050000_i128;
    if elapsed > 200 {
        let bid_modifier = if elapsed < 400 {
            SCALAR_7
                .checked_sub(
                    i128::from(elapsed - 200)
                        .checked_mul(per_ledger)
                        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError)),
                )
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
        } else {
            0
        };
        (bid_modifier, SCALAR_7)
    } else {
        (
            SCALAR_7,
            i128::from(elapsed)
                .checked_mul(per_ledger)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError)),
        )
    }
}

fn require_unique_bid_assets(e: &Env, bid: &Vec<Address>) {
    let mut assets = Map::<Address, bool>::new(e);
    for asset in bid {
        if assets.contains_key(asset.clone()) {
            panic_with_error!(e, PoolError::BadRequest);
        }
        assets.set(asset, true);
    }
}

fn convert_backstop_quote(quote: BackstopContractBadDebtLotQuote) -> BadDebtLotQuote {
    BadDebtLotQuote {
        committed_value: quote.committed_value,
        debt_value: quote.debt_value,
        tier: match quote.tier {
            BackstopContractTier::BlndUsdc => BackstopTier::BlndUsdc,
            BackstopContractTier::BlndXlm => BackstopTier::BlndXlm,
            BackstopContractTier::Usdc => BackstopTier::Usdc,
        },
        lot_amount: quote.lot_amount,
        unfilled_target_value: quote.unfilled_target_value,
        target_value: quote.target_value,
        valid_until: quote.valid_until,
    }
}

#[allow(dead_code)]
pub fn create_bad_debt_auction_data(
    e: &Env,
    user: &Address,
    bid: &Vec<Address>,
    lot: &Vec<Address>,
    percent: u32,
) -> AuctionData {
    let backstop = storage::get_backstop(e);
    if user != &backstop {
        panic_with_error!(e, PoolError::BadRequest);
    }
    if percent != 100 {
        panic_with_error!(e, PoolError::BadRequest);
    }
    if has_prepared_bad_debt_auction(e)
        || storage::has_auction(e, &(AuctionType::BadDebtAuction as u32), &backstop)
    {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let mut auction_data = AuctionData {
        bid: map![e],
        lot: map![e],
        block: e.ledger().sequence() + 1,
    };

    // validate and create bid auction data
    let mut pool = Pool::load(e);
    // lot is required to have 1 entry, so require bid to have less than max_positions entries
    if pool.config.max_positions <= bid.len() {
        panic_with_error!(e, PoolError::MaxPositionsExceeded);
    }

    let oracle_scalar = 10i128.pow(pool.load_price_decimals(e));
    let backstop_positions = storage::get_user_positions(e, &backstop);
    let mut debt_value = 0;
    for bid_asset in bid {
        let reserve = pool.load_reserve(e, &bid_asset, false);
        let liability_balance = backstop_positions
            .liabilities
            .get(reserve.config.index)
            .unwrap_or(0);
        if liability_balance > 0 {
            let asset_to_base = pool.load_price(e, &reserve.asset);
            let asset_balance = reserve.to_asset_from_d_token(e, liability_balance);
            debt_value += i128(asset_to_base).fixed_mul_floor(e, &asset_balance, &reserve.scalar);
            auction_data.bid.set(reserve.asset, liability_balance);
        } else {
            panic_with_error!(e, PoolError::InvalidBid);
        }
    }

    if auction_data.bid.is_empty() || debt_value <= 0 {
        panic_with_error!(e, PoolError::InvalidBid);
    }

    // validate and create lot auction data
    let backstop_client = BackstopClient::new(e, &backstop);
    let backstop_token = backstop_client.backstop_token();
    if lot.len() != 1 || lot.get_unchecked(0) != backstop_token {
        panic_with_error!(e, PoolError::InvalidLot);
    }

    // get value of backstop_token (BLND-USDC LP token) to base
    let pool_backstop_data = backstop_client.pool_data(&e.current_contract_address());

    if pool_backstop_data.tokens <= 0 {
        // no tokens left in backstop to auction off
        panic_with_error!(e, PoolError::InvalidLot);
    }

    // determine lot amount of backstop tokens needed to safely cover bad debt, or post
    // all backstop tokens if there isn't enough to cover the bad debt. backstop tokens use 7 decimals
    let mut lot_amount =
        debt_value // oracle_scalar
            .fixed_mul_floor(e, &1_2000000, &oracle_scalar) // denom of oracle_scalar means result is SCALAR_7
            .fixed_div_floor(e, &pool_backstop_data.token_spot_price, &SCALAR_7); // token_spot_price is SCALAR_7
    lot_amount = pool_backstop_data.tokens.min(lot_amount);
    auction_data.lot.set(backstop_token, lot_amount);

    auction_data
}

#[allow(clippy::inconsistent_digit_grouping)]
#[allow(dead_code)]
pub fn fill_bad_debt_auction(
    e: &Env,
    pool: &mut Pool,
    auction_data: &AuctionData,
    filler_state: &mut User,
    is_full_fill: bool,
) {
    let backstop_address = storage::get_backstop(e);
    if filler_state.address == backstop_address {
        panic_with_error!(e, PoolError::BadRequest);
    }
    let mut backstop_state = User::load(e, &backstop_address);

    // bid only contains d_token asset amounts
    backstop_state.rm_positions(e, pool, map![e], auction_data.bid.clone());
    filler_state.add_positions(e, pool, map![e], auction_data.bid.clone());

    let backstop_client = BackstopClient::new(e, &backstop_address);
    let backstop_token_id = backstop_client.backstop_token();
    let lot_amount = auction_data.lot.get(backstop_token_id).unwrap_or(0);
    if lot_amount > 0 {
        backstop_client.draw(
            &e.current_contract_address(),
            &lot_amount,
            &filler_state.address,
        );
    }

    if is_full_fill {
        // defaults rest of bad debt if insufficient backstop tokens remain in the backstop
        check_and_handle_backstop_bad_debt(e, pool, &backstop_address, &mut backstop_state);
    }
    backstop_state.store(e);
}

#[cfg(test)]
mod tests {

    use crate::{
        auctions::auction::AuctionType,
        pool::Positions,
        storage::PoolConfig,
        testutils::{self, create_pool},
    };

    use super::*;
    use sep_40_oracle::testutils::Asset;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        vec, Error, Symbol,
    };

    #[test]
    #[should_panic(expected = "Error(Contract, #1212)")]
    fn test_create_bad_debt_auction_already_in_progress() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        let pool_address = create_pool(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let auction_data = AuctionData {
            bid: map![&e],
            lot: map![&e],
            block: 50,
        };
        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e],
                &vec![&e, lp_token.clone()],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_create_bad_debt_auction_user_not_backstop() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        let pool_address = create_pool(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&pool_address, || {
            create_bad_debt_auction_data(&e, &samwise, &vec![&e], &vec![&e, lp_token.clone()], 100);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_create_bad_debt_auction_percent_not_100() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        let pool_address = create_pool(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let auction_data = AuctionData {
            bid: map![&e],
            lot: map![&e],
            block: 50,
        };
        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e],
                &vec![&e, lp_token.clone()],
                99,
            );
        });
    }

    #[test]
    #[should_panic]
    fn test_create_bad_debt_auction_invalid_bid() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config_0.index, 10_0000000),],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, lp_token.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1221)")]
    fn test_create_bad_debt_auction_invalid_bid_no_position() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config_0.index, 10_0000000),],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1221)")]
    fn test_create_bad_debt_auction_invalid_bid_empty() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config_0.index, 10_0000000),],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e],
                &vec![&e, lp_token.clone()],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1222)")]
    fn test_create_bad_debt_auction_invalid_lot() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config_0.index, 10_0000000),],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone()],
                &vec![&e, underlying_0.clone()],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1222)")]
    fn test_create_bad_debt_auction_no_backstop_tokens() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, _) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config_0.index, 10_0000000),],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1208)")]
    fn test_create_bad_debt_auction_checks_max_positions() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2.clone()),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000),
                (reserve_config_2.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 3,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![
                    &e,
                    underlying_0.clone(),
                    underlying_1.clone(),
                    underlying_2.clone(),
                ],
                &vec![&e, lp_token.clone()],
                100,
            );
        });
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
        let auction_id = BytesN::from_array(&e, &[7; 32]);
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);
            storage::set_user_positions(&e, &filler, &filler_positions);
        });
        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let auction = pool_client.new_bad_debt_auction(&auction_id, &vec![&e, debt_asset.clone()]);

        assert_eq!(auction.auction_id, auction_id);
        assert_eq!(auction.bid.get(debt_asset.clone()), Some(50 * SCALAR_7));
        assert_eq!(auction.lot_quote.debt_value, 50 * SCALAR_7);
        assert_eq!(auction.lot_quote.target_value, 60 * SCALAR_7);
        assert_eq!(auction.lot_quote.tier, crate::BackstopTier::BlndUsdc);
        assert_eq!(auction.lot_quote.lot_amount, 60 * SCALAR_7);
        assert_eq!(
            backstop_client.pool_bad_debt_commitment_count(&pool_address),
            1
        );
        assert_eq!(
            pool_client.backstop_loss_state(),
            crate::BackstopLossState {
                committed_loss_entries: 1,
                liability_entries: 1,
                unresolved_bad_debt_entries: 0,
            }
        );
        assert_eq!(
            pool_client.try_bad_debt(&backstop_address).err(),
            Some(Ok(Error::from_contract_error(1200)))
        );

        assert!(pool_client.try_fill_bad_debt_auction(&filler, &50).is_err());
        e.ledger().set_sequence_number(auction.block + 100);
        let auction_before_failed_fill = pool_client.get_bad_debt_auction();
        assert!(pool_client
            .try_fill_bad_debt_auction(&unhealthy_filler, &50)
            .is_err());
        assert_eq!(
            pool_client.get_bad_debt_auction(),
            auction_before_failed_fill
        );
        assert_eq!(
            backstop_client.pool_bad_debt_commitment_count(&pool_address),
            1
        );
        assert!(pool_client
            .get_positions(&unhealthy_filler)
            .liabilities
            .is_empty());

        let fill = pool_client.fill_bad_debt_auction(&filler, &50);
        assert_eq!(
            fill,
            BadDebtAuctionFill {
                auction_id: auction_id.clone(),
                base_lot_amount: 30 * SCALAR_7,
                bid: map![&e, (debt_asset.clone(), 25 * SCALAR_7)],
                block: auction.block,
                complete: false,
                lot_amount: 15 * SCALAR_7,
                tier: BackstopTier::BlndUsdc,
            }
        );
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
            pool_client.get_bad_debt_auction(),
            BadDebtAuctionData {
                auction_id: auction_id.clone(),
                bid: map![&e, (debt_asset.clone(), 25 * SCALAR_7)],
                block: auction.block,
                lot_quote: BadDebtLotQuote {
                    committed_value: 30 * SCALAR_7,
                    debt_value: 50 * SCALAR_7,
                    tier: BackstopTier::BlndUsdc,
                    lot_amount: 30 * SCALAR_7,
                    unfilled_target_value: 0,
                    target_value: 60 * SCALAR_7,
                    valid_until: u64::MAX,
                },
            }
        );
        assert_eq!(
            backstop_client
                .bad_debt_commitment(&pool_address, &auction_id)
                .unwrap()
                .lot_amount,
            30 * SCALAR_7
        );
        assert_eq!(
            pool_client.backstop_loss_state(),
            crate::BackstopLossState {
                committed_loss_entries: 1,
                liability_entries: 1,
                unresolved_bad_debt_entries: 0,
            }
        );

        e.ledger().set_sequence_number(auction.block + 500);
        pool_client.delete_stale_bad_debt_auction();
        assert!(!e.as_contract(&pool_address, || has_prepared_bad_debt_auction(&e)));
        assert_eq!(
            backstop_client.pool_bad_debt_commitment_count(&pool_address),
            0
        );
        assert_eq!(pool_client.backstop_loss_state().committed_loss_entries, 0);

        let discounted_auction_id = BytesN::from_array(&e, &[9; 32]);
        let discounted =
            pool_client.new_bad_debt_auction(&discounted_auction_id, &vec![&e, debt_asset.clone()]);
        e.ledger().set_sequence_number(discounted.block + 300);
        let discounted_fill = pool_client.fill_bad_debt_auction(&filler, &100);
        assert!(discounted_fill.complete);
        assert_eq!(
            discounted_fill.bid.get(debt_asset.clone()),
            Some(12_5000000)
        );
        assert_eq!(discounted_fill.lot_amount, 30 * SCALAR_7);
        assert!(pool_client.try_get_bad_debt_auction().is_err());
        assert_eq!(
            pool_client
                .get_positions(&backstop_address)
                .liabilities
                .get(reserve_config.index),
            Some(12_5000000)
        );
        assert_eq!(
            pool_client.backstop_loss_state(),
            crate::BackstopLossState {
                committed_loss_entries: 0,
                liability_entries: 1,
                unresolved_bad_debt_entries: 0,
            }
        );
        assert!(!pool_client.backstop_withdrawal_allowed(&backstop_address));

        let continuation_id = BytesN::from_array(&e, &[10; 32]);
        let continuation = pool_client.continue_bad_debt_resolution(&continuation_id);
        assert!(continuation.auction_created);
        assert!(continuation.defaulted.is_empty());
        let continued = pool_client.get_bad_debt_auction();
        assert_eq!(continued.auction_id, continuation_id);
        assert_eq!(continued.lot_quote.tier, BackstopTier::BlndUsdc);
        assert_eq!(
            continued.bid.get(debt_asset),
            Some(12_5000000),
            "the waterfall must reuse an eligible earlier tier"
        );
        assert!(pool_client
            .try_continue_bad_debt_resolution(&BytesN::from_array(&e, &[11; 32]))
            .is_err());

        e.ledger().set_sequence_number(continued.block + 200);
        assert!(pool_client.fill_bad_debt_auction(&filler, &100).complete);
        assert!(pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());
        assert_eq!(
            pool_client.backstop_loss_state(),
            crate::BackstopLossState {
                committed_loss_entries: 0,
                liability_entries: 0,
                unresolved_bad_debt_entries: 0,
            }
        );
        assert!(pool_client.backstop_withdrawal_allowed(&backstop_address));
    }

    #[test]
    fn continuation_defaults_only_after_verified_tier_exhaustion() {
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
        // Keep a real but sub-threshold tier balance so continuation must
        // obtain a valid quote before proving the tier ineligible.
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
            sync_backstop_liabilities(&e, &backstop_positions);
        });

        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let reserve_before = pool_client.get_reserve(&debt_asset).data;
        let valuation = mock_backstop_valuation::MockBackstopValuationClient::new(
            &e,
            &backstop_client.backstop_valuation(),
        );
        valuation.set_quote_failure(&true);
        assert!(pool_client
            .try_continue_bad_debt_resolution(&BytesN::from_array(&e, &[12; 32]))
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
        assert!(pool_client.try_get_bad_debt_auction().is_err());

        valuation.set_quote_failure(&false);
        let continuation =
            pool_client.continue_bad_debt_resolution(&BytesN::from_array(&e, &[13; 32]));
        assert!(!continuation.auction_created);
        assert_eq!(continuation.defaulted, map![&e, (debt_asset.clone(), debt)]);
        assert!(pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());
        let reserve_after = pool_client.get_reserve(&debt_asset).data;
        assert_eq!(reserve_after.d_supply, reserve_before.d_supply - debt);
        assert_eq!(reserve_after.b_supply, reserve_before.b_supply);
        assert_eq!(reserve_after.b_rate, SCALAR_7 as i128 * 50_000);
        assert_eq!(
            pool_client.backstop_loss_state(),
            crate::BackstopLossState {
                committed_loss_entries: 0,
                liability_entries: 0,
                unresolved_bad_debt_entries: 0,
            }
        );
        assert!(pool_client.backstop_withdrawal_allowed(&backstop_address));

        let dust = 1_i128;
        let dust_positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config.index, dust)],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            let mut reserve_data = storage::get_res_data(&e, &debt_asset);
            reserve_data.d_supply += dust;
            storage::set_res_data(&e, &debt_asset, &reserve_data);
            storage::set_user_positions(&e, &backstop_address, &dust_positions);
            sync_backstop_liabilities(&e, &dust_positions);
        });
        oracle_client.set_price_stable(&vec![&e, 1]);

        let dust_continuation =
            pool_client.continue_bad_debt_resolution(&BytesN::from_array(&e, &[16; 32]));
        assert!(!dust_continuation.auction_created);
        assert_eq!(
            dust_continuation.defaulted,
            map![&e, (debt_asset.clone(), dust)]
        );
        assert!(pool_client
            .get_positions(&backstop_address)
            .liabilities
            .is_empty());
        assert!(pool_client.backstop_withdrawal_allowed(&backstop_address));
    }

    #[test]
    fn continuation_caps_bid_and_rejects_unknown_liability_positions() {
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
            sync_backstop_liabilities(&e, &positions);
        });

        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let continuation =
            pool_client.continue_bad_debt_resolution(&BytesN::from_array(&e, &[14; 32]));
        assert!(continuation.auction_created);
        let auction = pool_client.get_bad_debt_auction();
        assert_eq!(auction.bid.len(), MAX_BAD_DEBT_BID_ASSETS);
        for index in 0..MAX_BAD_DEBT_BID_ASSETS {
            assert_eq!(auction.bid.get(assets.get(index).unwrap()), Some(SCALAR_7));
        }
        assert_eq!(auction.bid.get(assets.get(4).unwrap()), None);

        e.ledger()
            .set_sequence_number(auction.block + AUCTION_STALE_LEDGERS);
        pool_client.delete_stale_bad_debt_auction();
        let mut corrupt_positions = positions;
        corrupt_positions.liabilities.set(99, SCALAR_7);
        e.as_contract(&pool_address, || {
            e.storage().persistent().set(
                &crate::storage::PoolDataKey::Positions(backstop_address.clone()),
                &corrupt_positions,
            );
        });
        assert!(pool_client
            .try_continue_bad_debt_resolution(&BytesN::from_array(&e, &[15; 32]))
            .is_err());
        assert!(pool_client.try_get_bad_debt_auction().is_err());
        assert_eq!(
            backstop_client.pool_bad_debt_commitment_count(&pool_address),
            0
        );
    }

    #[test]
    fn test_create_bad_debt_auction() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 3,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let result = create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );

            assert_eq!(result.block, 51);
            assert_eq!(result.bid.get_unchecked(underlying_0), 10_0000000);
            assert_eq!(result.bid.get_unchecked(underlying_1), 2_5000000);
            assert_eq!(result.bid.len(), 2);
            assert_eq!(result.lot.get_unchecked(lp_token), 32_6400000);
            assert_eq!(result.lot.len(), 1);
        });
    }

    #[test]
    fn test_create_bad_debt_auction_oracle_14_decimals() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2),
                Asset::Stellar(usdc),
            ],
            &14,
            &300,
        );
        oracle_client.set_price_stable(&vec![
            &e,
            2_0000000_0000000,
            4_0000000_0000000,
            100_0000000_0000000,
            1_0000000_0000000,
        ]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let result = create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );

            assert_eq!(result.block, 51);
            assert_eq!(result.bid.get_unchecked(underlying_0), 10_0000000);
            assert_eq!(result.bid.get_unchecked(underlying_1), 2_5000000);
            assert_eq!(result.bid.len(), 2);
            assert_eq!(result.lot.get_unchecked(lp_token), 32_6400000);
            assert_eq!(result.lot.len(), 1);
        });
    }

    #[test]
    fn test_create_bad_debt_auction_oracle_2_decimals() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2),
                Asset::Stellar(usdc),
            ],
            &2,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_00, 4_00, 100_00, 1_00]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let result = create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );

            assert_eq!(result.block, 51);
            assert_eq!(result.bid.get_unchecked(underlying_0), 10_0000000);
            assert_eq!(result.bid.get_unchecked(underlying_1), 2_5000000);
            assert_eq!(result.bid.len(), 2);
            assert_eq!(result.lot.get_unchecked(lp_token), 32_6400000);
            assert_eq!(result.lot.len(), 1);
        });
    }

    #[test]
    fn test_create_bad_debt_auction_max_balance() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let pool_address = create_pool(&e);
        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens - only deposit 32_0000000
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &32_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);

            storage::set_user_positions(&e, &backstop_address, &positions);

            let result = create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );

            assert_eq!(result.block, 51);
            assert_eq!(result.bid.get_unchecked(underlying_0), 10_0000000);
            assert_eq!(result.bid.get_unchecked(underlying_1), 2_5000000);
            assert_eq!(result.bid.len(), 2);
            assert_eq!(result.lot.get_unchecked(lp_token), 32_0000000);
            assert_eq!(result.lot.len(), 1);
        });
    }

    #[test]
    fn test_create_bad_debt_auction_applies_interest() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 150,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 11845;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 11845;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 11845;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_backstop(&e, &backstop_address);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let result = create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );

            assert_eq!(result.block, 151);
            assert_eq!(result.bid.get_unchecked(underlying_0), 10_0000000);
            assert_eq!(result.bid.get_unchecked(underlying_1), 2_5000000);
            assert_eq!(result.bid.len(), 2);
            assert_eq!(result.lot.get_unchecked(lp_token), 32_6401624);
            assert_eq!(result.lot.len(), 1);
        });
    }

    #[test]
    fn test_create_bad_debt_auction_partial() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 50,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2),
                Asset::Stellar(usdc),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let result = create_bad_debt_auction_data(
                &e,
                &backstop_address,
                &vec![&e, underlying_0.clone()],
                &vec![&e, lp_token.clone()],
                100,
            );

            assert_eq!(result.block, 51);
            assert_eq!(result.bid.get_unchecked(underlying_0), 10_0000000);
            assert_eq!(result.bid.len(), 1);
            assert_eq!(result.lot.get_unchecked(lp_token), 21_1200000);
            assert_eq!(result.lot.len(), 1);
        });
    }

    #[test]
    fn test_fill_bad_debt_auction() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 51,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let mut auction_data = AuctionData {
            bid: map![&e, (underlying_0, 10_0000000), (underlying_1, 2_5000000)],
            lot: map![&e, (lp_token.clone(), 47_6000000)],
            block: 51,
        };
        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let mut pool = Pool::load(&e);
            let mut samwise_state = User::load(&e, &samwise);
            fill_bad_debt_auction(&e, &mut pool, &mut auction_data, &mut samwise_state, true);
            assert_eq!(
                lp_token_client.balance(&backstop_address),
                50_000_0000000 - 47_6000000
            );
            assert_eq!(lp_token_client.balance(&samwise), 47_6000000);
            let samwise_positions = samwise_state.positions;
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                10_0000000
            );
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                2_5000000
            );
            let backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(backstop_positions.liabilities.len(), 0);
        });
    }

    #[test]
    fn test_fill_bad_debt_auction_leftover_debt_small_backstop_burns() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 51,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &1_000_0000000,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let mut auction_data = AuctionData {
            bid: map![
                &e,
                (underlying_0.clone(), 10_0000000 - 2_5000000),
                (underlying_1.clone(), 2_5000000 - 6250000)
            ],
            lot: map![&e, (lp_token.clone(), 47_6000000)],
            block: 51,
        };
        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let pre_fill_d_supply_0 = reserve_data_0.d_supply;
            let pre_fill_d_supply_1 = reserve_data_1.d_supply;
            let pre_fill_b_rate_0 = reserve_data_0.b_rate;
            let pre_fill_b_rate_1 = reserve_data_1.b_rate;
            let mut pool = Pool::load(&e);
            let mut samwise_state = User::load(&e, &samwise);
            fill_bad_debt_auction(&e, &mut pool, &mut auction_data, &mut samwise_state, true);
            assert_eq!(
                lp_token_client.balance(&backstop_address),
                1_000_0000000 - 47_6000000
            );
            assert_eq!(
                lp_token_client.balance(&samwise),
                50_000_0000000 - 1_000_0000000 + 47_6000000
            );
            let samwise_positions = samwise_state.positions;
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                10_0000000 - 2_5000000
            );
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                2_5000000 - 0_6250000
            );
            let backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(backstop_positions.liabilities.len(), 0);
            assert_eq!(backstop_positions.collateral.len(), 0);
            assert_eq!(backstop_positions.supply.len(), 0);

            // verify reserve data is updated and set to be stored
            pool.store_cached_reserves(&e);
            let reserve_data_0 = storage::get_res_data(&e, &underlying_0);
            assert_eq!(reserve_data_0.d_supply, pre_fill_d_supply_0 - 2_5000000);
            assert!(reserve_data_0.b_rate < pre_fill_b_rate_0);
            let reserve_data_1 = storage::get_res_data(&e, &underlying_1);
            assert_eq!(reserve_data_1.d_supply, pre_fill_d_supply_1 - 0_6250000);
            assert!(reserve_data_1.b_rate < pre_fill_b_rate_1);
        });
    }

    #[test]
    fn test_fill_bad_debt_auction_leftover_debt_small_backstop_does_not_burn_if_not_full_liq() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 51,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &1_000_0000000,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let mut auction_data = AuctionData {
            bid: map![
                &e,
                (underlying_0.clone(), 10_0000000 - 2_5000000),
                (underlying_1.clone(), 2_5000000 - 6250000)
            ],
            lot: map![&e, (lp_token.clone(), 47_6000000)],
            block: 51,
        };
        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let mut pool = Pool::load(&e);
            let mut samwise_state = User::load(&e, &samwise);
            fill_bad_debt_auction(&e, &mut pool, &mut auction_data, &mut samwise_state, false);
            assert_eq!(
                lp_token_client.balance(&backstop_address),
                1_000_0000000 - 47_6000000
            );
            assert_eq!(
                lp_token_client.balance(&samwise),
                50_000_0000000 - 1_000_0000000 + 47_6000000
            );
            let samwise_positions = samwise_state.positions;
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                10_0000000 - 2_5000000
            );
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                2_5000000 - 0_6250000
            );
            let backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(backstop_positions.liabilities.len(), 2);
            assert_eq!(backstop_positions.collateral.len(), 0);
            assert_eq!(backstop_positions.supply.len(), 0);
            assert_eq!(
                backstop_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                10_0000000 - (10_0000000 - 2_5000000)
            );
            assert_eq!(
                backstop_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                2_5000000 - (2_5000000 - 0_6250000)
            );
        });
    }

    #[test]
    fn test_fill_bad_debt_auction_leftover_debt_sufficient_balance() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 51,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &2_500_0000000,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let mut auction_data = AuctionData {
            bid: map![
                &e,
                (underlying_0.clone(), 10_0000000 - 2_5000000),
                (underlying_1.clone(), 2_5000000 - 6250000)
            ],
            lot: map![&e, (lp_token.clone(), 47_6000000)],
            block: 51,
        };
        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let pre_fill_d_supply_0 = reserve_data_0.d_supply;
            let pre_fill_d_supply_1 = reserve_data_1.d_supply;
            let pre_fill_b_rate_0 = reserve_data_0.b_rate;
            let pre_fill_b_rate_1 = reserve_data_1.b_rate;
            let mut pool = Pool::load(&e);
            let mut samwise_state = User::load(&e, &samwise);
            fill_bad_debt_auction(&e, &mut pool, &mut auction_data, &mut samwise_state, true);
            assert_eq!(
                lp_token_client.balance(&backstop_address),
                2_500_0000000 - 47_6000000
            );
            assert_eq!(
                lp_token_client.balance(&samwise),
                50_000_0000000 - 2_500_0000000 + 47_6000000
            );
            let samwise_positions = samwise_state.positions;
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                10_0000000 - 2_5000000
            );
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                2_5000000 - 6250000
            );
            let backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(
                backstop_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                2_5000000
            );
            assert_eq!(
                backstop_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                6250000
            );

            // verify reserve data is updated and set to be stored
            pool.store_cached_reserves(&e);
            let reserve_data_0 = storage::get_res_data(&e, &underlying_0);
            assert_eq!(reserve_data_0.d_supply, pre_fill_d_supply_0);
            assert_eq!(reserve_data_0.b_rate, pre_fill_b_rate_0);
            let reserve_data_1 = storage::get_res_data(&e, &underlying_1);
            assert_eq!(reserve_data_1.d_supply, pre_fill_d_supply_1);
            assert_eq!(reserve_data_1.b_rate, pre_fill_b_rate_1);
        });
    }

    #[test]
    fn test_fill_bad_debt_auction_empty_bid() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 51,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let mut auction_data = AuctionData {
            bid: map![&e],
            lot: map![&e, (lp_token.clone(), 47_6000000)],
            block: 51,
        };
        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let mut pool = Pool::load(&e);
            let mut samwise_state = User::load(&e, &samwise);
            fill_bad_debt_auction(&e, &mut pool, &mut auction_data, &mut samwise_state, true);
            assert_eq!(
                lp_token_client.balance(&backstop_address),
                50_000_0000000 - 47_6000000
            );
            assert_eq!(lp_token_client.balance(&samwise), 47_6000000);
            let samwise_positions = samwise_state.positions;
            assert_eq!(samwise_positions.liabilities.len(), 0);
            let backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(
                backstop_positions
                    .liabilities
                    .get(reserve_config_0.index)
                    .unwrap(),
                10_0000000
            );
            assert_eq!(
                backstop_positions
                    .liabilities
                    .get(reserve_config_1.index)
                    .unwrap(),
                2_5000000
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_fill_bad_debt_auction_with_backstop() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited(); // setup exhausts budget

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 51,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (blnd, blnd_client) = testutils::create_blnd_token(&e, &pool_address, &bombadil);
        let (usdc, usdc_client) = testutils::create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) =
            testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);
        // mint lp tokens
        blnd_client.mint(&samwise, &500_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &12_501_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_address,
            &50_000_0000000,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.d_rate = 1_100_000_000_000;
        reserve_data_0.last_time = 12345;
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, mut reserve_data_1) = testutils::default_reserve_meta();
        reserve_data_1.d_rate = 1_200_000_000_000;
        reserve_data_1.last_time = 12345;
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, mut reserve_data_2) = testutils::default_reserve_meta();
        reserve_data_2.b_rate = 1_100_000_000_000;
        reserve_data_2.last_time = 12345;
        reserve_config_2.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let mut auction_data = AuctionData {
            bid: map![&e, (underlying_0, 10_0000000), (underlying_1, 2_5000000)],
            lot: map![&e, (lp_token.clone(), 47_6000000)],
            block: 51,
        };
        let positions: Positions = Positions {
            collateral: map![&e],
            liabilities: map![
                &e,
                (reserve_config_0.index, 10_0000000),
                (reserve_config_1.index, 2_5000000)
            ],
            supply: map![&e],
        };

        e.as_contract(&pool_address, || {
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction_data,
            );
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &positions);

            let mut pool = Pool::load(&e);
            let mut backstop_state = User::load(&e, &backstop_address);
            fill_bad_debt_auction(&e, &mut pool, &mut auction_data, &mut backstop_state, true);
        });
    }
}
