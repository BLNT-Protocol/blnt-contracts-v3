use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopContractBadDebtLotQuote, BackstopContractTier},
    errors::PoolError,
    pool::{backstop_liability, check_and_handle_backstop_bad_debt, Pool, User},
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

/// The fixed v3 backstop tiers in first-loss order.
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

/// Prepared single-tier bad-debt auction data. Filling is ported separately.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BadDebtAuctionData {
    pub auction_id: BytesN<32>,
    pub bid: Map<Address, i128>,
    pub block: u32,
    pub lot_quote: BadDebtLotQuote,
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

    let debt_value_usdc = debt_value.fixed_mul_ceil(e, &SCALAR_7, &oracle_scalar);
    if bid_amounts.is_empty() || debt_value_usdc <= 0 {
        panic_with_error!(e, PoolError::InvalidBid);
    }

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

    let block = e
        .ledger()
        .sequence()
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let auction = BadDebtAuctionData {
        auction_id: auction_id.clone(),
        bid: bid_amounts,
        block,
        lot_quote,
    };
    set_prepared_bad_debt_auction(e, &auction);
    auction
}

pub fn get_prepared_bad_debt_auction(e: &Env) -> BadDebtAuctionData {
    e.storage()
        .temporary()
        .get(&PreparedBadDebtDataKey::Auction)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest))
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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
    fn prepared_bad_debt_auction_commits_and_stale_delete_releases_lot() {
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

        blnd_client.mint(&depositor, &500_001_0000000);
        blnd_client.approve(&depositor, &lp_token, &i128::MAX, &99_999);
        usdc_client.mint(&depositor, &12_501_0000000);
        usdc_client.approve(&depositor, &lp_token, &i128::MAX, &99_999);
        lp_token_client.join_pool(
            &50_000_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &depositor,
        );
        backstop_client.deposit_blnd_usdc(&depositor, &pool_address, &50_000_0000000);

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

        let positions = Positions {
            collateral: map![&e],
            liabilities: map![&e, (reserve_config.index, 50 * SCALAR_7)],
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
            storage::set_user_positions(&e, &backstop_address, &positions);
        });
        let pool_client = crate::PoolClient::new(&e, &pool_address);
        let auction = pool_client.new_bad_debt_auction(&auction_id, &vec![&e, debt_asset.clone()]);

        assert_eq!(auction.auction_id, auction_id);
        assert_eq!(auction.bid.get(debt_asset), Some(50 * SCALAR_7));
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
            Some(Ok(Error::from_contract_error(1212)))
        );

        e.ledger().set_sequence_number(auction.block + 500);
        pool_client.delete_stale_bad_debt_auction();
        assert!(!e.as_contract(&pool_address, || has_prepared_bad_debt_auction(&e)));
        assert_eq!(
            backstop_client.pool_bad_debt_commitment_count(&pool_address),
            0
        );
        assert_eq!(pool_client.backstop_loss_state().committed_loss_entries, 0);
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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &32_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &1_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &1_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &2_500_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
        backstop_client.deposit_blnd_usdc(&samwise, &pool_address, &50_000_0000000);

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
