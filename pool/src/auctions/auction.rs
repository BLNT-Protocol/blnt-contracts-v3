use crate::{
    dependencies::BackstopContractTier,
    errors::PoolError,
    pool::{Pool, User},
    storage,
};
use cast::i128;
use soroban_sdk::{contracttype, map, panic_with_error, Address, Env, Map, Vec};

use super::{
    backstop_interest_auction::{
        create_interest_auction_data, fill_interest_auction, get_interest_auction,
    },
    bad_debt_auction::{create_bad_debt_auction_data, fill_bad_debt_auction, get_bad_debt_auction},
    math::{auction_modifiers, scale_bid_amount, scale_lot_amount},
    user_liquidation_auction::{create_user_liq_auction_data, fill_user_liq_auction},
};

#[derive(Clone, PartialEq)]
#[repr(u32)]
pub enum AuctionType {
    UserLiquidation = 0,
    BadDebtAuction = 1,
    InterestAuction = 2,
}

impl AuctionType {
    pub fn from_u32(e: &Env, value: u32) -> Self {
        match value {
            0 => Self::UserLiquidation,
            1 => Self::BadDebtAuction,
            2 => Self::InterestAuction,
            _ => panic_with_error!(e, PoolError::BadRequest),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AuctionData {
    /// A map of the assets being bid on and the amount being bid. These are tokens spent
    /// by the filler of the auction.
    ///
    /// The bid is different based on each auction type:
    /// - UserLiquidation: dTokens
    /// - BadDebtAuction: dTokens
    /// - InterestAuction: The selected backstop token
    pub bid: Map<Address, i128>,
    /// A map of the assets being auctioned off and the amount being auctioned. These are tokens
    /// received by the filler of the auction.
    ///
    /// The lot is different based on each auction type:
    /// - UserLiquidation: bTokens
    /// - BadDebtAuction: Underlying assets (backstop token)
    /// - InterestAuction: Underlying assets
    pub lot: Map<Address, i128>,
    /// The block the auction begins on. This is used to determine how the auction
    /// should be scaled based on the number of blocks that have passed since the auction began.
    pub block: u32,
}

/// Create a new auction. Stores the resulting auction to begin on the next ledger.
///
/// For bad debt, `bid = []` or `lot = []` accepts the corresponding canonical
/// asset set; a nonempty vector asserts an exact match. For interest, `bid = []`
/// accepts the canonically selected tier token and a nonempty `bid` asserts it;
/// `lot` must be a nonempty reserve-asset input, so `lot = []` is invalid.
/// Liquidation keeps the inherited requirement that both vectors are nonempty
/// inputs.
/// Assertions do not influence asset selection, amounts, or pricing.
///
/// Returns the AuctionData object created
///
/// ### Arguments
/// * `auction_type` - The type of auction being created
/// * `user` - The user involved in the auction
/// * `bid` - The assets being bid on
/// * `lot` - The assets being auctioned off
/// * `percent` - The percentage of the user's positions being liquidated
///
/// ### Panics
/// * If the max positions are exceeded
/// * If the user or percentage is invalid for the auction type
/// * If a nonempty bad-debt or interest assertion does not exactly match the canonical asset set
/// * If the auction is unable to be created
pub fn create_auction(
    e: &Env,
    auction_type: u32,
    user: &Address,
    bid: &Vec<Address>,
    lot: &Vec<Address>,
    percent: u32,
) -> AuctionData {
    require_unique_addresses(e, bid);
    require_unique_addresses(e, lot);
    match AuctionType::from_u32(e, auction_type) {
        AuctionType::UserLiquidation => {
            let auction = create_user_liq_auction_data(e, user, bid, lot, percent);
            storage::set_auction(e, &auction_type, user, &auction);
            auction
        }
        AuctionType::BadDebtAuction => {
            require_backstop_auction_args(e, user, percent);
            let auction = create_bad_debt_auction_data(e).auction;
            require_optional_asset_match(e, bid, &auction.bid, PoolError::InvalidBid);
            require_optional_asset_match(e, lot, &auction.lot, PoolError::InvalidLot);
            auction
        }
        AuctionType::InterestAuction => {
            require_backstop_auction_args(e, user, percent);
            let auction = create_interest_auction_data(e, lot).auction;
            require_optional_asset_match(e, bid, &auction.bid, PoolError::InvalidBid);
            auction
        }
    }
}

/// Fetch an auction from its canonical v2-compatible public scope.
pub fn get_auction(e: &Env, auction_type: u32, user: &Address) -> AuctionData {
    match AuctionType::from_u32(e, auction_type) {
        AuctionType::UserLiquidation => storage::get_auction(e, &auction_type, user),
        AuctionType::BadDebtAuction => {
            require_backstop_auction_user(e, user);
            get_bad_debt_auction(e).auction
        }
        AuctionType::InterestAuction => {
            require_backstop_auction_user(e, user);
            get_interest_auction(e).auction
        }
    }
}

/// Delete an auction if it is stale.
pub fn delete_stale_auction(e: &Env, auction_type: u32, user: &Address) {
    match AuctionType::from_u32(e, auction_type) {
        AuctionType::UserLiquidation => {
            if !storage::has_auction(e, &auction_type, user) {
                panic_with_error!(e, PoolError::BadRequest);
            }
            let auction = storage::get_auction(e, &auction_type, user);
            if auction.block + AUCTION_STALE_LEDGERS > e.ledger().sequence() {
                panic_with_error!(e, PoolError::BadRequest);
            }
            storage::del_auction(e, &auction_type, user);
        }
        kind @ (AuctionType::BadDebtAuction | AuctionType::InterestAuction) => {
            require_backstop_auction_user(e, user);
            del_tier_auction(e, kind);
        }
    }
}

/// Delete a liquidation auction if the user being liquidated
///
/// NOTE: Does not verify if the user's positions are healthy. This must be done
/// before the contract call is completed.
///
/// ### Arguments
/// * `auction_type` - The type of auction being created
///
/// ### Panics
/// If no auction exists for the user
pub fn delete_liquidation(e: &Env, user: &Address) {
    if !storage::has_auction(e, &(AuctionType::UserLiquidation as u32), user) {
        panic_with_error!(e, PoolError::BadRequest);
    }
    storage::del_auction(e, &(AuctionType::UserLiquidation as u32), user);
}

/// Fill an auction from the invoker.
///
/// ### Arguments
/// * `pool` - The pool
/// * `auction_type` - The type of auction to fill
/// * `user` - The user involved in the auction
/// * `filler_state` - The Address filling the auction
/// * `percent_filled` - The percentage being filled as a number (i.e. 15 => 15%)
///
/// ### Panics
/// If the auction does not exist, or if the pool is unable to fulfill either side
/// of the auction quote
pub fn fill(
    e: &Env,
    pool: &mut Pool,
    auction_type: u32,
    user: &Address,
    filler_state: &mut User,
    percent_filled: u64,
) -> AuctionData {
    match AuctionType::from_u32(e, auction_type) {
        AuctionType::UserLiquidation => {
            if user.clone() == filler_state.address {
                panic_with_error!(e, PoolError::InvalidLiquidation);
            }
            let auction_data = storage::get_auction(e, &auction_type, user);
            let (to_fill_auction, remaining_auction) =
                scale_auction(e, &auction_data, percent_filled);
            let is_full_fill = remaining_auction.is_none();
            fill_user_liq_auction(e, pool, &to_fill_auction, user, filler_state, is_full_fill);
            if let Some(auction_to_store) = remaining_auction {
                storage::set_auction(e, &auction_type, user, &auction_to_store);
            } else {
                storage::del_auction(e, &auction_type, user);
            }
            to_fill_auction
        }
        AuctionType::BadDebtAuction => {
            fill_bad_debt_auction(e, pool, user, filler_state, fill_percent(e, percent_filled))
        }
        AuctionType::InterestAuction => {
            fill_interest_auction(e, pool, user, filler_state, fill_percent(e, percent_filled))
        }
    }
}

fn fill_percent(e: &Env, percent: u64) -> u32 {
    u32::try_from(percent)
        .ok()
        .filter(|value| *value > 0 && *value <= 100)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest))
}

fn require_backstop_auction_user(e: &Env, user: &Address) {
    if user != &storage::get_backstop(e) {
        panic_with_error!(e, PoolError::BadRequest);
    }
}

fn require_backstop_auction_args(e: &Env, user: &Address, percent: u32) {
    require_backstop_auction_user(e, user);
    if percent != 100 {
        panic_with_error!(e, PoolError::BadRequest);
    }
}

/// Require a nonempty caller-supplied asset set to match the canonical auction
/// side exactly. The assertion never selects assets or controls amounts.
fn require_optional_asset_match(
    e: &Env,
    expected: &Vec<Address>,
    actual: &Map<Address, i128>,
    error: PoolError,
) {
    if expected.is_empty() {
        return;
    }
    if expected.len() != actual.len() {
        panic_with_error!(e, error);
    }
    for asset in expected {
        if !actual.contains_key(asset) {
            panic_with_error!(e, error);
        }
    }
}

/// Scale the auction based on the percent being filled and the amount of blocks that have passed
/// since the auction began.
///
/// ### Arguments
/// * `auction_data` - The auction data to scale
/// * `percent_filled` - The percentage being filled as a number (i.e. 15 => 15%)
///
/// Returns the (Scaled Auction, Remaining Auction) such that:
/// - Scaled Auction is the auction data scaled
/// - Remaining Auction is the leftover auction data that will be stored in the ledger, or deleted if None
///
/// ### Panics
/// If the percent filled is greater than 100 or less than 0
fn scale_auction(
    e: &Env,
    auction_data: &AuctionData,
    percent_filled: u64,
) -> (AuctionData, Option<AuctionData>) {
    if percent_filled > 100 || percent_filled == 0 {
        panic_with_error!(e, PoolError::BadRequest);
    }

    let mut to_fill_auction = AuctionData {
        bid: map![e],
        lot: map![e],
        block: auction_data.block,
    };
    let mut remaining_auction = AuctionData {
        bid: map![e],
        lot: map![e],
        block: auction_data.block,
    };

    // Preserve v2's checked-arithmetic VM trap when a fill is attempted
    // before its start ledger.
    let elapsed = e.ledger().sequence() - auction_data.block;
    let (bid_modifier, lot_modifier) = auction_modifiers(e, elapsed);

    // scale the auction
    let percent_filled_i128 = i128(percent_filled) * 1_00000; // scale to decimal form in 7 decimals from percentage
    for (asset, amount) in auction_data.bid.iter() {
        let (_, to_fill_scaled, remaining_base) =
            scale_bid_amount(e, amount, percent_filled_i128, bid_modifier);
        if remaining_base > 0 {
            remaining_auction.bid.set(asset.clone(), remaining_base);
        }
        if to_fill_scaled > 0 {
            to_fill_auction.bid.set(asset, to_fill_scaled);
        }
    }
    for (asset, amount) in auction_data.lot.iter() {
        let (_, to_fill_scaled, remaining_base) =
            scale_lot_amount(e, amount, percent_filled_i128, lot_modifier);
        if remaining_base > 0 {
            remaining_auction.lot.set(asset.clone(), remaining_base);
        }
        if to_fill_scaled > 0 {
            to_fill_auction.lot.set(asset, to_fill_scaled);
        }
    }

    if remaining_auction.lot.is_empty() && remaining_auction.bid.is_empty() {
        (to_fill_auction, None)
    } else {
        (to_fill_auction, Some(remaining_auction))
    }
}

/// Require that all addresses in the list are unique
///
/// ### Panics
/// If any duplicate addresses are found
pub(crate) fn require_unique_addresses(e: &Env, list: &Vec<Address>) {
    let mut temp_map = Map::<Address, bool>::new(e);
    for address in list {
        if temp_map.contains_key(address.clone()) {
            panic_with_error!(e, PoolError::BadRequest);
        }
        temp_map.set(address.clone(), true);
    }
}

/// The immutable loss-waterfall positions used by pool auctions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub enum BackstopTier {
    FirstLoss,
    SecondLoss,
    ThirdLoss,
}

/// One active auction backed by a single v3 backstop tier.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
pub(crate) struct TierAuctionData {
    pub auction: AuctionData,
    pub tier: BackstopTier,
}

#[derive(Clone)]
#[contracttype]
enum TierAuctionDataKey {
    Auction(u32),
}

const ONE_DAY_LEDGERS: u32 = 17_280;
const TIER_AUCTION_TTL_THRESHOLD: u32 = 45 * ONE_DAY_LEDGERS;
const TIER_AUCTION_TTL_BUMP: u32 = 46 * ONE_DAY_LEDGERS;
pub(crate) const AUCTION_STALE_LEDGERS: u32 = 500;

pub(crate) fn get_tier_auction(e: &Env, auction_type: AuctionType) -> TierAuctionData {
    e.storage()
        .temporary()
        .get(&TierAuctionDataKey::Auction(auction_type as u32))
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest))
}

pub(crate) fn has_tier_auction(e: &Env, auction_type: AuctionType) -> bool {
    e.storage()
        .temporary()
        .has(&TierAuctionDataKey::Auction(auction_type as u32))
}

pub(crate) fn set_tier_auction(e: &Env, auction_type: AuctionType, auction: &TierAuctionData) {
    let key = TierAuctionDataKey::Auction(auction_type as u32);
    e.storage().temporary().set(&key, auction);
    e.storage()
        .temporary()
        .extend_ttl(&key, TIER_AUCTION_TTL_THRESHOLD, TIER_AUCTION_TTL_BUMP);
}

pub(crate) fn del_tier_auction(e: &Env, auction_type: AuctionType) {
    let auction = get_tier_auction(e, auction_type.clone());
    let stale_at = auction
        .auction
        .block
        .checked_add(AUCTION_STALE_LEDGERS)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if e.ledger().sequence() < stale_at {
        panic_with_error!(e, PoolError::BadRequest);
    }
    remove_tier_auction(e, auction_type);
}

pub(crate) fn remove_tier_auction(e: &Env, auction_type: AuctionType) {
    e.storage()
        .temporary()
        .remove(&TierAuctionDataKey::Auction(auction_type as u32));
}

pub(crate) fn to_backstop_tier(tier: BackstopTier) -> BackstopContractTier {
    match tier {
        BackstopTier::SecondLoss => BackstopContractTier::SecondLoss,
        BackstopTier::FirstLoss => BackstopContractTier::FirstLoss,
        BackstopTier::ThirdLoss => BackstopContractTier::ThirdLoss,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        constants::SCALAR_7,
        pool::Positions,
        storage::PoolConfig,
        testutils::{self, create_comet_lp_pool, create_pool},
    };

    use super::*;
    use sep_40_oracle::testutils::Asset;
    use soroban_sdk::{
        map,
        testutils::{Address as _, Ledger, LedgerInfo},
        unwrap::UnwrapOptimized,
        vec, Symbol,
    };

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
            &backstop::BackstopTier::SecondLoss,
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
        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        oracle_client.set_data(
            &bombadil,
            &Asset::Other(Symbol::new(&e, "USD1")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2.clone()),
                Asset::Stellar(usdc),
                Asset::Stellar(blnd),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![
            &e,
            2_0000000,
            4_0000000,
            100_0000000,
            1_0000000,
            0_1000000,
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

            let created = create_auction(&e, 1, &backstop_address, &vec![&e], &vec![&e], 100);
            assert_eq!(get_auction(&e, 1, &backstop_address), created);
        });
    }

    #[test]
    fn test_create_interest_auction() {
        let e = Env::default();
        e.mock_all_auths();
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

        let pool_address = create_pool(&e);
        let (usdc_id, _) = testutils::create_token_contract(&e, &bombadil);
        let (blnd_id, _) = testutils::create_blnd_token(&e, &pool_address, &bombadil);

        let (backstop_token_id, _) = create_comet_lp_pool(&e, &bombadil, &blnd_id, &usdc_id);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &backstop_token_id, &usdc_id, &blnd_id);
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &bombadil,
            &pool_address,
            &(50 * SCALAR_7),
        );
        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.last_time = 12345;
        reserve_data_0.backstop_credit = 100_0000000;
        reserve_data_0.b_supply = 1000_0000000;
        reserve_data_0.d_supply = 750_0000000;
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
        reserve_data_1.backstop_credit = 25_0000000;
        reserve_data_1.b_supply = 250_0000000;
        reserve_data_1.d_supply = 187_5000000;
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
                Asset::Stellar(usdc_id),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);

            let created = create_auction(
                &e,
                2,
                &backstop_address,
                &vec![&e],
                &vec![&e, underlying_0, underlying_1],
                100,
            );
            assert_eq!(get_auction(&e, 2, &backstop_address), created);
        });
    }

    #[test]
    fn test_create_liquidation() {
        let e = Env::default();

        e.mock_all_auths();
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
        let (oracle_address, oracle_client) = testutils::create_mock_oracle(&e);

        // creating reserves for a pool exhausts the budget
        e.cost_estimate().budget().reset_unlimited();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.last_time = 12345;
        reserve_data_0.b_rate = 1_100_000_000_000;
        reserve_config_0.c_factor = 0_8500000;
        reserve_config_0.l_factor = 0_9000000;
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
        reserve_data_1.b_rate = 1_200_000_000_000;
        reserve_config_1.c_factor = 0_7500000;
        reserve_config_1.l_factor = 0_7500000;
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
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.c_factor = 0_0000000;
        reserve_config_2.l_factor = 0_7000000;
        reserve_config_2.index = 2;
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
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 50_0000000]);

        let liq_pct = 45;
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_backstop(&e, &Address::generate(&e));
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);

            e.cost_estimate().budget().reset_unlimited();
            create_auction(
                &e,
                0,
                &samwise,
                &vec![&e, underlying_2],
                &vec![&e, underlying_0, underlying_1],
                liq_pct,
            );
            assert!(storage::has_auction(&e, &0, &samwise));
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1211)")]
    fn test_create_liquidation_for_pool() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();
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

        let pool_address = create_pool(&e);
        let (oracle_address, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.last_time = 12345;
        reserve_data_0.b_rate = 1_100_000_000_000;
        reserve_config_0.c_factor = 0_8500000;
        reserve_config_0.l_factor = 0_9000000;
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
        reserve_data_1.b_rate = 1_200_000_000_000;
        reserve_config_1.c_factor = 0_7500000;
        reserve_config_1.l_factor = 0_7500000;
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
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.c_factor = 0_0000000;
        reserve_config_2.l_factor = 0_7000000;
        reserve_config_2.index = 2;
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
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 50_0000000]);

        let liq_pct = 45;
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_backstop(&e, &Address::generate(&e));
            storage::set_user_positions(&e, &pool_address, &positions);
            storage::set_pool_config(&e, &pool_config);

            create_auction(
                &e,
                0,
                &pool_address,
                &vec![&e, underlying_2],
                &vec![&e, underlying_0, underlying_1],
                liq_pct,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1211)")]
    fn test_create_liquidation_for_backstop() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();
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

        let pool_address = create_pool(&e);
        let backstop = Address::generate(&e);
        let (oracle_address, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.last_time = 12345;
        reserve_data_0.b_rate = 1_100_000_000_000;
        reserve_config_0.c_factor = 0_8500000;
        reserve_config_0.l_factor = 0_9000000;
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
        reserve_data_1.b_rate = 1_200_000_000_000;
        reserve_config_1.c_factor = 0_7500000;
        reserve_config_1.l_factor = 0_7500000;
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
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.c_factor = 0_0000000;
        reserve_config_2.l_factor = 0_7000000;
        reserve_config_2.index = 2;
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
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 50_0000000]);

        let liq_pct = 45;
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_backstop(&e, &backstop);
            storage::set_user_positions(&e, &backstop, &positions);
            storage::set_pool_config(&e, &pool_config);

            create_auction(
                &e,
                0,
                &backstop,
                &vec![&e, underlying_2],
                &vec![&e, underlying_0, underlying_1],
                liq_pct,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_create_user_liquidation_auction_duplicate_bid() {
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
            &backstop::BackstopTier::SecondLoss,
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
            &Asset::Other(Symbol::new(&e, "USD1")),
            &vec![
                &e,
                Asset::Stellar(underlying_0.clone()),
                Asset::Stellar(underlying_1.clone()),
                Asset::Stellar(underlying_2.clone()),
                Asset::Stellar(usdc),
                Asset::Stellar(blnd),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![
            &e,
            2_0000000,
            4_0000000,
            100_0000000,
            1_0000000,
            0_1000000,
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

            create_auction(
                &e,
                0,
                &backstop_address,
                &vec![&e, underlying_0.clone(), underlying_1, underlying_0],
                &vec![&e, lp_token],
                100,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_create_user_liquidation_auction_duplicate_lot() {
        let e = Env::default();
        e.mock_all_auths();
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

        let pool_address = create_pool(&e);
        let (usdc_id, _) = testutils::create_token_contract(&e, &bombadil);
        let (blnd_id, _) = testutils::create_blnd_token(&e, &pool_address, &bombadil);

        let (backstop_token_id, _) = create_comet_lp_pool(&e, &bombadil, &blnd_id, &usdc_id);
        let (backstop_address, backstop_client) =
            testutils::create_backstop(&e, &pool_address, &backstop_token_id, &usdc_id, &blnd_id);
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &bombadil,
            &pool_address,
            &(50 * SCALAR_7),
        );
        let (oracle_id, oracle_client) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, mut reserve_data_0) = testutils::default_reserve_meta();
        reserve_data_0.last_time = 12345;
        reserve_data_0.backstop_credit = 100_0000000;
        reserve_data_0.b_supply = 1000_0000000;
        reserve_data_0.d_supply = 750_0000000;
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
        reserve_data_1.backstop_credit = 25_0000000;
        reserve_data_1.b_supply = 250_0000000;
        reserve_data_1.d_supply = 187_5000000;
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
                Asset::Stellar(usdc_id),
            ],
            &7,
            &300,
        );
        oracle_client.set_price_stable(&vec![&e, 2_0000000, 4_0000000, 100_0000000, 1_0000000]);

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_address, || {
            storage::set_pool_config(&e, &pool_config);

            create_auction(
                &e,
                0,
                &backstop_address,
                &vec![&e, backstop_token_id],
                &vec![&e, underlying_0.clone(), underlying_1, underlying_0],
                100,
            );
        });
    }

    #[test]
    fn test_delete_user_liquidation() {
        let e = Env::default();
        e.mock_all_auths();

        let pool_id = create_pool(&e);
        let samwise = Address::generate(&e);

        let auction_data = AuctionData {
            bid: map![&e],
            lot: map![&e],
            block: 100,
        };
        e.as_contract(&pool_id, || {
            storage::set_auction(
                &e,
                &(AuctionType::UserLiquidation as u32),
                &samwise,
                &auction_data,
            );

            delete_liquidation(&e, &samwise);
            assert!(!storage::has_auction(
                &e,
                &(AuctionType::UserLiquidation as u32),
                &samwise
            ));
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_delete_user_liquidation_does_not_exist() {
        let e = Env::default();
        e.mock_all_auths();
        let pool_id = create_pool(&e);

        let samwise = Address::generate(&e);

        e.as_contract(&pool_id, || {
            delete_liquidation(&e, &samwise);
        });
    }

    #[test]
    fn test_fill() {
        let e = Env::default();

        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 175,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (oracle_address, _) = testutils::create_mock_oracle(&e);

        // creating reserves for a pool exhausts the budget
        e.cost_estimate().budget().reset_unlimited();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, reserve_data_0) = testutils::default_reserve_meta();
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, reserve_data_1) = testutils::default_reserve_meta();
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        e.cost_estimate().budget().reset_unlimited();

        let auction_data = AuctionData {
            bid: map![&e, (underlying_2.clone(), 1_2375000)],
            lot: map![
                &e,
                (underlying_0.clone(), 30_5595329),
                (underlying_1.clone(), 1_5395739)
            ],
            block: 176,
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);
            storage::set_auction(&e, &0, &samwise, &auction_data);

            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 200 * 5,
                protocol_version: 27,
                sequence_number: 176 + 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            e.cost_estimate().budget().reset_unlimited();
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 100);
            let has_auction = storage::has_auction(&e, &0, &samwise);
            assert_eq!(has_auction, false);
        });
    }

    #[test]
    fn test_partial_fill() {
        let e = Env::default();

        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 175,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (oracle_address, _) = testutils::create_mock_oracle(&e);

        // creating reserves for a pool exhausts the budget
        e.cost_estimate().budget().reset_unlimited();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, reserve_data_0) = testutils::default_reserve_meta();
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, reserve_data_1) = testutils::default_reserve_meta();
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        e.cost_estimate().budget().reset_unlimited();

        let auction_data = AuctionData {
            bid: map![&e, (underlying_2.clone(), 1_2375000)],
            lot: map![
                &e,
                (underlying_0.clone(), 30_5595329),
                (underlying_1.clone(), 1_5395739)
            ],
            block: 176,
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);
            storage::set_auction(&e, &0, &samwise, &auction_data);

            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 200 * 5,
                protocol_version: 27,
                sequence_number: 176 + 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            e.cost_estimate().budget().reset_unlimited();
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 25);

            let expected_new_auction_data = AuctionData {
                bid: map![&e, (underlying_2.clone(), 9281250)],
                lot: map![
                    &e,
                    (underlying_0.clone(), 22_9196497),
                    (underlying_1.clone(), 1_1546805)
                ],
                block: 176,
            };
            let new_auction = storage::get_auction(&e, &0, &samwise);
            assert_eq!(new_auction.bid, expected_new_auction_data.bid);
            assert_eq!(new_auction.lot, expected_new_auction_data.lot);
            assert_eq!(new_auction.block, expected_new_auction_data.block);
        });
    }

    #[test]
    fn test_partial_partial_full_fill() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 175,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (oracle_address, _) = testutils::create_mock_oracle(&e);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, reserve_data_0) = testutils::default_reserve_meta();

        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, reserve_data_1) = testutils::default_reserve_meta();

        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();

        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        let auction_data = AuctionData {
            bid: map![&e, (underlying_2.clone(), 100_000_0000)],
            lot: map![
                &e,
                (underlying_0.clone(), 10_000_0000),
                (underlying_1.clone(), 1_000_0000)
            ],
            block: 176,
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 30_000_0000),
                (reserve_config_1.index, 3_000_0000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 200_000_0000),],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);
            storage::set_auction(&e, &0, &samwise, &auction_data);

            // Partial fill 1 - 25% @ 50% lot mod
            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 100 * 5,
                protocol_version: 27,
                sequence_number: 176 + 100,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 25);

            let expected_new_auction_data = AuctionData {
                bid: map![&e, (underlying_2.clone(), 75_000_0000)],
                lot: map![
                    &e,
                    (underlying_0.clone(), 7_500_0000),
                    (underlying_1.clone(), 750_0000)
                ],
                block: 176,
            };

            // Partial fill 2 - 66% @ 100% mods
            let new_auction = storage::get_auction(&e, &0, &samwise);
            assert_eq!(new_auction.bid, expected_new_auction_data.bid);
            assert_eq!(new_auction.lot, expected_new_auction_data.lot);
            assert_eq!(new_auction.block, expected_new_auction_data.block);

            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 200 * 5,
                protocol_version: 27,
                sequence_number: 176 + 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 67);

            let expected_new_auction_data = AuctionData {
                bid: map![&e, (underlying_2.clone(), 24_7500000)],
                lot: map![
                    &e,
                    (underlying_0.clone(), 2_4750000),
                    (underlying_1.clone(), 0_2475000)
                ],
                block: 176,
            };
            let new_auction = storage::get_auction(&e, &0, &samwise);
            assert_eq!(new_auction.bid, expected_new_auction_data.bid);
            assert_eq!(new_auction.lot, expected_new_auction_data.lot);
            assert_eq!(new_auction.block, expected_new_auction_data.block);

            // full fill at 50% bid mod
            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 300 * 5,
                protocol_version: 27,
                sequence_number: 176 + 300,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 100);
            let new_auction = storage::has_auction(&e, &0, &samwise);
            assert_eq!(new_auction, false);
            let samwise_positions = storage::get_user_positions(&e, &samwise);
            assert_eq!(
                samwise_positions
                    .collateral
                    .get(reserve_config_0.index)
                    .unwrap_optimized(),
                30_000_0000 - 1_250_0000 - 5_000_0002 - 2_499_9998
            );
            assert_eq!(
                samwise_positions
                    .collateral
                    .get(reserve_config_1.index)
                    .unwrap_optimized(),
                3_000_0000 - 125_0000 - 500_0000 - 250_0000
            );
            assert_eq!(
                samwise_positions
                    .liabilities
                    .get(reserve_config_2.index)
                    .unwrap_optimized(),
                200_000_0000 - 25_000_0000 - 50_000_0025 - 12_6249975
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_fill_fails_pct_too_large() {
        let e = Env::default();

        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 175,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (oracle_address, _) = testutils::create_mock_oracle(&e);

        // creating reserves for a pool exhausts the budget
        e.cost_estimate().budget().reset_unlimited();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, reserve_data_0) = testutils::default_reserve_meta();
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, reserve_data_1) = testutils::default_reserve_meta();
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );

        let auction_data = AuctionData {
            bid: map![&e, (underlying_2.clone(), 1_2375000)],
            lot: map![
                &e,
                (underlying_0.clone(), 30_5595329),
                (underlying_1.clone(), 1_5395739)
            ],
            block: 176,
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);
            storage::set_auction(&e, &0, &samwise, &auction_data);

            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 200 * 5,
                protocol_version: 27,
                sequence_number: 176 + 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            e.cost_estimate().budget().reset_unlimited();
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 101);

            let expected_new_auction_data = AuctionData {
                bid: map![&e, (underlying_2.clone(), 9281250)],
                lot: map![
                    &e,
                    (underlying_0.clone(), 22_9196497),
                    (underlying_1.clone(), 1_1546805)
                ],
                block: 176,
            };
            let new_auction = storage::get_auction(&e, &0, &samwise);
            assert_eq!(new_auction.bid, expected_new_auction_data.bid);
            assert_eq!(new_auction.lot, expected_new_auction_data.lot);
            assert_eq!(new_auction.block, expected_new_auction_data.block);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_fill_fails_pct_too_small() {
        let e = Env::default();

        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 175,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (oracle_address, _) = testutils::create_mock_oracle(&e);

        // creating reserves for a pool exhausts the budget
        e.cost_estimate().budget().reset_unlimited();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, reserve_data_0) = testutils::default_reserve_meta();
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, reserve_data_1) = testutils::default_reserve_meta();

        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();

        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        e.cost_estimate().budget().reset_unlimited();
        let auction_data = AuctionData {
            bid: map![&e, (underlying_2.clone(), 1_2375000)],
            lot: map![
                &e,
                (underlying_0.clone(), 30_5595329),
                (underlying_1.clone(), 1_5395739)
            ],
            block: 176,
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);
            storage::set_auction(&e, &0, &samwise, &auction_data);

            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 200 * 5,
                protocol_version: 27,
                sequence_number: 176 + 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            e.cost_estimate().budget().reset_unlimited();
            let mut pool = Pool::load(&e);
            let mut frodo_state = User::load(&e, &frodo);
            fill(&e, &mut pool, 0, &samwise, &mut frodo_state, 0);

            let expected_new_auction_data = AuctionData {
                bid: map![&e, (underlying_2.clone(), 9281250)],
                lot: map![
                    &e,
                    (underlying_0.clone(), 22_9196497),
                    (underlying_1.clone(), 1_1546805)
                ],
                block: 176,
            };
            let new_auction = storage::get_auction(&e, &0, &samwise);
            assert_eq!(new_auction.bid, expected_new_auction_data.bid);
            assert_eq!(new_auction.lot, expected_new_auction_data.lot);
            assert_eq!(new_auction.block, expected_new_auction_data.block);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1211)")]
    fn test_fill_liquidation_same_address() {
        let e = Env::default();

        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 175,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let pool_address = create_pool(&e);

        let (oracle_address, _) = testutils::create_mock_oracle(&e);

        // creating reserves for a pool exhausts the budget
        e.cost_estimate().budget().reset_unlimited();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_0, reserve_data_0) = testutils::default_reserve_meta();
        reserve_config_0.index = 0;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_0,
            &reserve_config_0,
            &reserve_data_0,
        );

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_1, reserve_data_1) = testutils::default_reserve_meta();
        reserve_config_1.index = 1;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_1,
            &reserve_config_1,
            &reserve_data_1,
        );

        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config_2, reserve_data_2) = testutils::default_reserve_meta();
        reserve_config_2.index = 2;
        testutils::create_reserve(
            &e,
            &pool_address,
            &underlying_2,
            &reserve_config_2,
            &reserve_data_2,
        );
        e.cost_estimate().budget().reset_unlimited();

        let auction_data = AuctionData {
            bid: map![&e, (underlying_2.clone(), 1_2375000)],
            lot: map![
                &e,
                (underlying_0.clone(), 30_5595329),
                (underlying_1.clone(), 1_5395739)
            ],
            block: 176,
        };
        let pool_config = PoolConfig {
            oracle: oracle_address,
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 0,
            max_positions: 4,
        };
        let positions: Positions = Positions {
            collateral: map![
                &e,
                (reserve_config_0.index, 90_9100000),
                (reserve_config_1.index, 04_5800000),
            ],
            liabilities: map![&e, (reserve_config_2.index, 02_7500000),],
            supply: map![&e],
        };
        e.as_contract(&pool_address, || {
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_pool_config(&e, &pool_config);
            storage::set_auction(&e, &0, &samwise, &auction_data);

            e.ledger().set(LedgerInfo {
                timestamp: 12345 + 200 * 5,
                protocol_version: 27,
                sequence_number: 176 + 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 172800,
                min_persistent_entry_ttl: 172800,
                max_entry_ttl: 9999999,
            });
            e.cost_estimate().budget().reset_unlimited();
            let mut pool = Pool::load(&e);
            let mut samwise_state = User::load(&e, &samwise);
            fill(&e, &mut pool, 0, &samwise, &mut samwise_state, 100);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1227)")]
    fn test_fill_interest_auction_same_address_uses_interest_error() {
        let e = Env::default();
        e.mock_all_auths();

        let pool_address = create_pool(&e);
        e.as_contract(&pool_address, || {
            let backstop = storage::get_backstop(&e);
            let mut pool = Pool::load(&e);
            let mut backstop_state = User::load(&e, &backstop);

            fill(
                &e,
                &mut pool,
                AuctionType::InterestAuction as u32,
                &backstop,
                &mut backstop_state,
                100,
            );
        });
    }

    #[test]
    fn test_delete_stale_auction() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1500,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let pool_address = create_pool(&e);
        let auction_type = AuctionType::UserLiquidation as u32;
        let user = Address::generate(&e);
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 100_0000000)],
            lot: map![&e, (underlying_1.clone(), 100_0000000)],
            block: 1000,
        };
        e.as_contract(&pool_address, || {
            storage::set_auction(&e, &auction_type, &user, &auction_data);
            let has_auction = storage::has_auction(&e, &auction_type, &user);
            assert_eq!(has_auction, true);

            delete_stale_auction(&e, auction_type, &user);
            let has_auction = storage::has_auction(&e, &auction_type, &user);
            assert_eq!(has_auction, false);
        });
    }

    // #[test]
    // fn test_delete_stale_auction_bad_debt() {
    //     let e = Env::default();
    //     e.mock_all_auths();

    //     e.ledger().set(LedgerInfo {
    //         timestamp: 12345,
    //         protocol_version: 27,
    //         sequence_number: 1500,
    //         network_id: Default::default(),
    //         base_reserve: 10,
    //         min_temp_entry_ttl: 172800,
    //         min_persistent_entry_ttl: 172800,
    //         max_entry_ttl: 9999999,
    //     });

    //     let pool_address = create_pool(&e);
    //     let bombadil = Address::generate(&e);
    //     let frodo = Address::generate(&e);

    //     let (blnd, blnd_client) = create_blnd_token(&e, &pool_address, &bombadil);
    //     let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
    //     let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
    //     let (backstop_address, backstop_client) =
    //         create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

    //     // mint lp tokens and deposit them into the pool's backstop
    //     let backstop_tokens = 1_500_0000000; // over 5% of threshold
    //     blnd_client.mint(&frodo, &500_001_0000000);
    //     blnd_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     usdc_client.mint(&frodo, &12_501_0000000);
    //     usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     lp_token_client.join_pool(
    //         &backstop_tokens,
    //         &vec![&e, 500_001_0000000, 12_501_0000000],
    //         &frodo,
    //     );
    //     backstop_client.deposit(&backstop::BackstopTier::SecondLoss, &frodo, &pool_address, &backstop_tokens);

    //     let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_0,
    //         &reserve_config,
    //         &reserve_data_0,
    //     );

    //     let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_1,
    //         &reserve_config,
    //         &reserve_data_1,
    //     );

    //     let auction_type: u32 = 1;
    //     let auction_data = AuctionData {
    //         bid: map![&e, (underlying_0.clone(), 100_0000000)],
    //         lot: map![&e, (underlying_1.clone(), 100_0000000)],
    //         block: 1000,
    //     };

    //     let backstop_positions = Positions {
    //         collateral: map![&e],
    //         liabilities: map![&e, (0, 100_0000000)],
    //         supply: map![&e,],
    //     };
    //     let pool_config = PoolConfig {
    //         oracle: Address::generate(&e),
    //         min_collateral: 1_0000000,
    //         bstop_rate: 0_1000000,
    //         status: 1,
    //         max_positions: 5,
    //     };
    //     e.as_contract(&pool_address, || {
    //         storage::set_pool_config(&e, &pool_config);
    //         storage::set_user_positions(&e, &backstop_address, &backstop_positions);
    //         storage::set_auction(&e, &auction_type, &backstop_address, &auction_data);
    //         let has_auction = storage::has_auction(&e, &auction_type, &backstop_address);
    //         assert_eq!(has_auction, true);

    //         delete_stale_auction(&e, auction_type, &backstop_address);
    //         let has_auction = storage::has_auction(&e, &auction_type, &backstop_address);
    //         assert_eq!(has_auction, false);

    //         // validate no other state changed
    //         let post_backstop_positions = storage::get_user_positions(&e, &backstop_address);
    //         assert_eq!(post_backstop_positions.collateral.len(), 0);
    //         assert_eq!(
    //             post_backstop_positions.liabilities,
    //             backstop_positions.liabilities
    //         );
    //         assert_eq!(post_backstop_positions.supply.len(), 0);

    //         let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
    //         assert_eq!(post_reserve_data_0.last_time, 0);
    //         assert_eq!(post_reserve_data_0.d_supply, reserve_data_0.d_supply);
    //         let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
    //         assert_eq!(post_reserve_data_1.last_time, 0);
    //         assert_eq!(post_reserve_data_1.d_supply, reserve_data_1.d_supply);
    //     });
    // }

    // #[test]
    // fn test_delete_stale_auction_bad_debt_needs_default() {
    //     let e = Env::default();
    //     e.mock_all_auths();

    //     e.ledger().set(LedgerInfo {
    //         timestamp: 12345,
    //         protocol_version: 27,
    //         sequence_number: 1500,
    //         network_id: Default::default(),
    //         base_reserve: 10,
    //         min_temp_entry_ttl: 172800,
    //         min_persistent_entry_ttl: 172800,
    //         max_entry_ttl: 9999999,
    //     });

    //     let pool_address = create_pool(&e);
    //     let bombadil = Address::generate(&e);
    //     let frodo = Address::generate(&e);

    //     let (blnd, blnd_client) = create_blnd_token(&e, &pool_address, &bombadil);
    //     let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
    //     let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
    //     let (backstop_address, backstop_client) =
    //         create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

    //     // mint lp tokens and deposit them into the pool's backstop
    //     let backstop_tokens = 1_000_0000000; // under 5% of threshold
    //     blnd_client.mint(&frodo, &500_001_0000000);
    //     blnd_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     usdc_client.mint(&frodo, &12_501_0000000);
    //     usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     lp_token_client.join_pool(
    //         &backstop_tokens,
    //         &vec![&e, 500_001_0000000, 12_501_0000000],
    //         &frodo,
    //     );
    //     backstop_client.deposit(&backstop::BackstopTier::SecondLoss, &frodo, &pool_address, &backstop_tokens);

    //     let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_0,
    //         &reserve_config,
    //         &reserve_data_0,
    //     );

    //     let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_1,
    //         &reserve_config,
    //         &reserve_data_1,
    //     );

    //     let auction_type: u32 = 1;
    //     let auction_data = AuctionData {
    //         bid: map![&e, (underlying_0.clone(), 100_0000000)],
    //         lot: map![&e, (underlying_1.clone(), 100_0000000)],
    //         block: 1000,
    //     };

    //     let backstop_positions = Positions {
    //         collateral: map![&e],
    //         liabilities: map![&e, (0, 100_0000000)],
    //         supply: map![&e,],
    //     };
    //     let pool_config = PoolConfig {
    //         oracle: Address::generate(&e),
    //         min_collateral: 1_0000000,
    //         bstop_rate: 0_1000000,
    //         status: 1,
    //         max_positions: 5,
    //     };
    //     e.as_contract(&pool_address, || {
    //         storage::set_pool_config(&e, &pool_config);
    //         storage::set_user_positions(&e, &backstop_address, &backstop_positions);
    //         storage::set_auction(&e, &auction_type, &backstop_address, &auction_data);
    //         let has_auction = storage::has_auction(&e, &auction_type, &backstop_address);
    //         assert_eq!(has_auction, true);

    //         delete_stale_auction(&e, auction_type, &backstop_address);
    //         let has_auction = storage::has_auction(&e, &auction_type, &backstop_address);
    //         assert_eq!(has_auction, false);

    //         // validate backstop positions defaulted
    //         let post_backstop_positions = storage::get_user_positions(&e, &backstop_address);
    //         assert_eq!(post_backstop_positions.collateral.len(), 0);
    //         assert_eq!(post_backstop_positions.liabilities.len(), 0);
    //         assert_eq!(post_backstop_positions.supply.len(), 0);

    //         let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
    //         assert_eq!(post_reserve_data_0.last_time, 12345);
    //         assert!(post_reserve_data_0.d_supply < reserve_data_0.d_supply);
    //         assert!(post_reserve_data_0.d_rate > reserve_data_0.d_rate);
    //         assert_eq!(post_reserve_data_0.b_supply, reserve_data_0.b_supply);
    //         assert!(post_reserve_data_0.b_rate < reserve_data_0.b_rate);
    //         // non-affected reserve not changed
    //         let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
    //         assert_eq!(post_reserve_data_1.last_time, 0);
    //         assert_eq!(post_reserve_data_1.d_supply, reserve_data_1.d_supply);
    //     });
    // }

    // #[test]
    // fn test_delete_stale_auction_user_liquidation() {
    //     let e = Env::default();
    //     e.mock_all_auths();

    //     e.ledger().set(LedgerInfo {
    //         timestamp: 12345,
    //         protocol_version: 27,
    //         sequence_number: 1500,
    //         network_id: Default::default(),
    //         base_reserve: 10,
    //         min_temp_entry_ttl: 172800,
    //         min_persistent_entry_ttl: 172800,
    //         max_entry_ttl: 9999999,
    //     });

    //     let pool_address = create_pool(&e);
    //     let bombadil = Address::generate(&e);
    //     let frodo = Address::generate(&e);
    //     let samwise = Address::generate(&e);

    //     let (blnd, blnd_client) = create_blnd_token(&e, &pool_address, &bombadil);
    //     let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
    //     let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
    //     let (_, backstop_client) = create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

    //     // mint lp tokens and deposit them into the pool's backstop
    //     let backstop_tokens = 1_500_0000000; // over 5% of threshold
    //     blnd_client.mint(&frodo, &500_001_0000000);
    //     blnd_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     usdc_client.mint(&frodo, &12_501_0000000);
    //     usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     lp_token_client.join_pool(
    //         &backstop_tokens,
    //         &vec![&e, 500_001_0000000, 12_501_0000000],
    //         &frodo,
    //     );
    //     backstop_client.deposit(&backstop::BackstopTier::SecondLoss, &frodo, &pool_address, &backstop_tokens);

    //     let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_0,
    //         &reserve_config,
    //         &reserve_data_0,
    //     );

    //     let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_1,
    //         &reserve_config,
    //         &reserve_data_1,
    //     );

    //     let auction_type: u32 = 0;
    //     let auction_data = AuctionData {
    //         bid: map![&e, (underlying_0.clone(), 100_0000000)],
    //         lot: map![&e, (underlying_1.clone(), 100_0000000)],
    //         block: 1000,
    //     };

    //     let positions = Positions {
    //         collateral: map![&e, (1, 100_0000000)],
    //         liabilities: map![&e, (0, 100_0000000)],
    //         supply: map![&e,],
    //     };
    //     let pool_config = PoolConfig {
    //         oracle: Address::generate(&e),
    //         min_collateral: 1_0000000,
    //         bstop_rate: 0_1000000,
    //         status: 1,
    //         max_positions: 5,
    //     };
    //     e.as_contract(&pool_address, || {
    //         storage::set_pool_config(&e, &pool_config);
    //         storage::set_user_positions(&e, &samwise, &positions);
    //         storage::set_auction(&e, &auction_type, &samwise, &auction_data);
    //         let has_auction = storage::has_auction(&e, &auction_type, &samwise);
    //         assert_eq!(has_auction, true);

    //         delete_stale_auction(&e, auction_type, &samwise);
    //         let has_auction = storage::has_auction(&e, &auction_type, &samwise);
    //         assert_eq!(has_auction, false);

    //         // validate no other state changed
    //         let post_positions = storage::get_user_positions(&e, &samwise);
    //         assert_eq!(post_positions.collateral, positions.collateral);
    //         assert_eq!(post_positions.liabilities, positions.liabilities);
    //         assert_eq!(post_positions.supply, positions.supply);

    //         let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
    //         assert_eq!(post_reserve_data_0.last_time, 0);
    //         assert_eq!(post_reserve_data_0.d_supply, reserve_data_0.d_supply);
    //         let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
    //         assert_eq!(post_reserve_data_1.last_time, 0);
    //         assert_eq!(post_reserve_data_1.d_supply, reserve_data_1.d_supply);
    //     });
    // }

    // #[test]
    // fn test_delete_stale_auction_user_liquidation_bad_debt() {
    //     let e = Env::default();
    //     e.mock_all_auths();

    //     e.ledger().set(LedgerInfo {
    //         timestamp: 12345,
    //         protocol_version: 27,
    //         sequence_number: 1500,
    //         network_id: Default::default(),
    //         base_reserve: 10,
    //         min_temp_entry_ttl: 172800,
    //         min_persistent_entry_ttl: 172800,
    //         max_entry_ttl: 9999999,
    //     });

    //     let pool_address = create_pool(&e);
    //     let bombadil = Address::generate(&e);
    //     let frodo = Address::generate(&e);
    //     let samwise = Address::generate(&e);

    //     let (blnd, blnd_client) = create_blnd_token(&e, &pool_address, &bombadil);
    //     let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
    //     let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
    //     let (backstop_address, backstop_client) =
    //         create_backstop(&e, &pool_address, &lp_token, &usdc, &blnd);

    //     // mint lp tokens and deposit them into the pool's backstop
    //     let backstop_tokens = 1_500_0000000; // over 5% of threshold
    //     blnd_client.mint(&frodo, &500_001_0000000);
    //     blnd_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     usdc_client.mint(&frodo, &12_501_0000000);
    //     usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
    //     lp_token_client.join_pool(
    //         &backstop_tokens,
    //         &vec![&e, 500_001_0000000, 12_501_0000000],
    //         &frodo,
    //     );
    //     backstop_client.deposit(&backstop::BackstopTier::SecondLoss, &frodo, &pool_address, &backstop_tokens);

    //     let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_0,
    //         &reserve_config,
    //         &reserve_data_0,
    //     );

    //     let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
    //     let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
    //     testutils::create_reserve(
    //         &e,
    //         &pool_address,
    //         &underlying_1,
    //         &reserve_config,
    //         &reserve_data_1,
    //     );

    //     let auction_type: u32 = 0;
    //     let auction_data = AuctionData {
    //         bid: map![&e, (underlying_0.clone(), 100_0000000)],
    //         lot: map![&e, (underlying_1.clone(), 100_0000000)],
    //         block: 1000,
    //     };

    //     let positions = Positions {
    //         collateral: map![&e],
    //         liabilities: map![&e, (0, 100_0000000)],
    //         supply: map![&e,],
    //     };
    //     let pool_config = PoolConfig {
    //         oracle: Address::generate(&e),
    //         min_collateral: 1_0000000,
    //         bstop_rate: 0_1000000,
    //         status: 1,
    //         max_positions: 5,
    //     };
    //     e.as_contract(&pool_address, || {
    //         storage::set_pool_config(&e, &pool_config);
    //         storage::set_user_positions(&e, &samwise, &positions);
    //         storage::set_auction(&e, &auction_type, &samwise, &auction_data);
    //         let has_auction = storage::has_auction(&e, &auction_type, &samwise);
    //         assert_eq!(has_auction, true);

    //         delete_stale_auction(&e, auction_type, &samwise);
    //         let has_auction = storage::has_auction(&e, &auction_type, &samwise);
    //         assert_eq!(has_auction, false);

    //         // validate bad debt assigned to backstop
    //         let post_positions = storage::get_user_positions(&e, &samwise);
    //         assert_eq!(post_positions.collateral.len(), 0);
    //         assert_eq!(post_positions.liabilities.len(), 0);
    //         assert_eq!(post_positions.supply.len(), 0);

    //         let backstop_positions = storage::get_user_positions(&e, &backstop_address);
    //         assert_eq!(backstop_positions.collateral.len(), 0);
    //         assert_eq!(backstop_positions.liabilities, positions.liabilities);
    //         assert_eq!(backstop_positions.supply.len(), 0);

    //         let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
    //         assert_eq!(post_reserve_data_0.last_time, 12345);
    //         assert_eq!(post_reserve_data_0.d_supply, reserve_data_0.d_supply);
    //         let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
    //         assert_eq!(post_reserve_data_1.last_time, 0);
    //         assert_eq!(post_reserve_data_1.d_supply, reserve_data_1.d_supply);
    //     });
    // }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_delete_stale_auction_not_stale() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1500,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let pool_address = create_pool(&e);
        let user = Address::generate(&e);
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let auction_type = AuctionType::UserLiquidation as u32;
        let auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 100_0000000)],
            lot: map![&e, (underlying_1.clone(), 100_0000000)],
            block: 1001,
        };

        e.as_contract(&pool_address, || {
            storage::set_auction(&e, &auction_type, &user, &auction_data);
            let has_auction = storage::has_auction(&e, &auction_type, &user);
            assert_eq!(has_auction, true);

            delete_stale_auction(&e, auction_type, &user);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_delete_stale_auction_does_not_exist() {
        let e = Env::default();
        e.mock_all_auths();

        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1500,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let pool_address = create_pool(&e);
        let user = Address::generate(&e);

        e.as_contract(&pool_address, || {
            delete_stale_auction(&e, AuctionType::UserLiquidation as u32, &user);
        });
    }

    #[test]
    fn test_scale_auction_100_fill_pct() {
        // 0 blocks
        let e = Env::default();
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let base_auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 100_0000000)],
            lot: map![&e, (underlying_1.clone(), 100_0000000)],
            block: 1000,
        };

        // 0 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            100_0000000
        );
        assert_eq!(scaled_auction.lot.len(), 0);
        assert!(remaining_auction.is_none());

        // 100 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            100_0000000
        );
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            50_0000000
        );
        assert!(remaining_auction.is_none());

        // 200 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1200,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            100_0000000
        );
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            100_0000000
        );
        assert!(remaining_auction.is_none());

        // 300 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1300,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            50_0000000
        );
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            100_0000000
        );
        assert!(remaining_auction.is_none());

        // 400 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1400,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(scaled_auction.bid.len(), 0);
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            100_0000000
        );
        assert!(remaining_auction.is_none());
    }

    #[test]
    fn test_scale_auction_not_100_fill_pct() {
        // @dev: bids always round up, lots always round down
        //       the remaining is exact based on scaled auction
        let e = Env::default();
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let base_auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 25_0000005)],
            lot: map![&e, (underlying_1.clone(), 25_0000005)],
            block: 1000,
        };

        // 0 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 50);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            12_5000003 // fill pct rounds up
        );
        assert_eq!(scaled_auction.lot.len(), 0);
        assert_eq!(
            remaining_auction.bid.get_unchecked(underlying_0.clone()),
            12_5000002
        );
        assert_eq!(
            remaining_auction.lot.get_unchecked(underlying_1.clone()),
            12_5000003
        );

        // 100 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 60);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            15_0000003
        );
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            7_5000001 // modifier rounds down
        );
        assert_eq!(
            remaining_auction.bid.get_unchecked(underlying_0.clone()),
            10_0000002
        );
        assert_eq!(
            remaining_auction.lot.get_unchecked(underlying_1.clone()),
            10_0000002
        );

        // 300 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1300,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 60);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(
            scaled_auction.bid.get_unchecked(underlying_0.clone()),
            7_5000002 // modifier rounds up
        );
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            15_0000003
        );
        assert_eq!(
            remaining_auction.bid.get_unchecked(underlying_0.clone()),
            10_0000002
        );
        assert_eq!(
            remaining_auction.lot.get_unchecked(underlying_1.clone()),
            10_0000002
        );

        // 400 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1400,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 50);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(scaled_auction.bid.len(), 0);
        assert_eq!(
            scaled_auction.lot.get_unchecked(underlying_1.clone()),
            12_5000002 // fill pct rounds down
        );
        assert_eq!(
            remaining_auction.bid.get_unchecked(underlying_0.clone()),
            12_5000002
        );
        assert_eq!(
            remaining_auction.lot.get_unchecked(underlying_1.clone()),
            12_5000003
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_scale_auction_fill_percentage_zero() {
        let e = Env::default();
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let base_auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 25_0000005)],
            lot: map![&e, (underlying_1.clone(), 25_0000005)],
            block: 1000,
        };

        // 0 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (_, _) = scale_auction(&e, &base_auction_data, 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_scale_auction_fill_percentage_over_100() {
        let e = Env::default();
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let base_auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 25_0000005)],
            lot: map![&e, (underlying_1.clone(), 25_0000005)],
            block: 1000,
        };

        // 0 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (_, _) = scale_auction(&e, &base_auction_data, 101);
    }

    #[test]
    fn test_scale_auction_dust() {
        // @dev: bids always round up, lots always round down
        //       the remaining is exact based on scaled auction
        let e = Env::default();
        let underlying_0 = Address::generate(&e);
        let underlying_1 = Address::generate(&e);

        let base_auction_data = AuctionData {
            bid: map![&e, (underlying_0.clone(), 0_0000001)],
            lot: map![&e, (underlying_1.clone(), 0_0000001)],
            block: 1000,
        };

        // 0 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 99);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.len(), 0);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(remaining_auction.bid.len(), 0);
        assert_eq!(remaining_auction.lot.get_unchecked(underlying_1.clone()), 1);

        // 100 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.len(), 0);
        assert!(remaining_auction_option.is_none());

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 99);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.len(), 0);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(remaining_auction.bid.len(), 0);
        assert_eq!(remaining_auction.lot.get_unchecked(underlying_1.clone()), 1);

        // 200 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1200,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 99);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.len(), 0);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(remaining_auction.bid.len(), 0);
        assert_eq!(remaining_auction.lot.get_unchecked(underlying_1.clone()), 1);

        // 300 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1300,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.get_unchecked(underlying_1.clone()), 1);
        assert!(remaining_auction_option.is_none());

        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 99);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.len(), 0);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(remaining_auction.bid.len(), 0);
        assert_eq!(remaining_auction.lot.get_unchecked(underlying_1.clone()), 1);

        // 399 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1399,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 99);
        assert_eq!(scaled_auction.bid.get_unchecked(underlying_0.clone()), 1);
        assert_eq!(scaled_auction.lot.len(), 0);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(remaining_auction.bid.len(), 0);
        assert_eq!(remaining_auction.lot.get_unchecked(underlying_1.clone()), 1);

        // 400 blocks
        e.ledger().set(LedgerInfo {
            timestamp: 12345,
            protocol_version: 27,
            sequence_number: 1400,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 172800,
            min_persistent_entry_ttl: 172800,
            max_entry_ttl: 9999999,
        });
        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 99);
        assert_eq!(scaled_auction.bid.len(), 0);
        assert_eq!(scaled_auction.lot.len(), 0);
        let remaining_auction = remaining_auction_option.unwrap();
        assert_eq!(remaining_auction.bid.len(), 0);
        assert_eq!(remaining_auction.lot.get_unchecked(underlying_1.clone()), 1);

        // with 100 fill pct
        let (scaled_auction, remaining_auction_option) = scale_auction(&e, &base_auction_data, 100);
        assert_eq!(scaled_auction.bid.len(), 0);
        assert_eq!(scaled_auction.lot.get_unchecked(underlying_1.clone()), 1);
        assert!(remaining_auction_option.is_none());
    }
}
