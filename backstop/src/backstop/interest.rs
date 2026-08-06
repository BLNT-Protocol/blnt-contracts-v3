use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, BytesN, Env, Map, I256};

use crate::{constants::SCALAR_7, emissions, errors::BackstopError, storage};

use super::{quote_lp_amount, require_registered_pool, tier_token, BackstopTier};

const ONE_DAY_LEDGERS: u32 = 17_280;
const AUCTION_TTL_THRESHOLD: u32 = 45 * ONE_DAY_LEDGERS;
const AUCTION_TTL_BUMP: u32 = 46 * ONE_DAY_LEDGERS;
const INTEREST_LOT_PREMIUM_NUMERATOR: i128 = 6;
const INTEREST_LOT_PREMIUM_DENOMINATOR: i128 = 5;
const TAKE_RATE_WEIGHT_BLND_XLM: i128 = 4;
const TAKE_RATE_WEIGHT_BLND_USDC: i128 = 3;
const TAKE_RATE_WEIGHT_USDC: i128 = 2;
const MAX_TAKE_RATE_BATCH: u32 = 4;

/// Canonical tier-token bid for one pool interest auction.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct InterestLotQuote {
    pub bid_token: Address,
    pub bid_amount: i128,
    pub lot_value: i128,
    pub tier: BackstopTier,
    pub target_value: i128,
    pub valid_until: u64,
}

/// Verified pool-tier values used for one take-rate allocation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct TakeRateValues {
    pub blnd_usdc: i128,
    pub blnd_xlm: i128,
    pub usdc: i128,
}

/// Canonical allocation for one reserve-credit amount:
/// BLND:XLM = 4, BLND:USDC = 3, and plain USDC = 2.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct TakeRateQuote {
    pub blnd_usdc: i128,
    pub blnd_xlm: i128,
    pub remainder: i128,
    pub usdc: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
struct InterestCommitment {
    auction_id: BytesN<32>,
    quote: InterestLotQuote,
}

#[derive(Clone)]
#[contracttype]
enum InterestDataKey {
    InterestLot(Address, BackstopTier),
}

/// Allocate a bounded reserve-credit batch using canonical pool-tier value.
pub(crate) fn quote_pool_take_rate_batch(
    e: &Env,
    pool: &Address,
    distributions: &Map<Address, i128>,
) -> Map<Address, TakeRateQuote> {
    require_registered_pool(e, pool);
    if distributions.is_empty() || distributions.len() > MAX_TAKE_RATE_BATCH {
        panic_with_error!(e, BackstopError::InvalidTakeRateValue);
    }
    let values = build_take_rate_values(e, pool);
    let mut quotes = Map::new(e);
    for (asset, distribution) in distributions.iter() {
        quotes.set(asset, quote_take_rate(e, distribution, &values));
    }
    quotes
}

/// Quote one reserve-credit allocation from already verified tier values.
pub(crate) fn quote_take_rate(
    e: &Env,
    distribution: i128,
    values: &TakeRateValues,
) -> TakeRateQuote {
    if distribution < 0 || values.blnd_usdc < 0 || values.blnd_xlm < 0 || values.usdc < 0 {
        panic_with_error!(e, BackstopError::InvalidTakeRateValue);
    }

    let blnd_usdc_weighted = checked_mul(e, values.blnd_usdc, TAKE_RATE_WEIGHT_BLND_USDC);
    let blnd_xlm_weighted = checked_mul(e, values.blnd_xlm, TAKE_RATE_WEIGHT_BLND_XLM);
    let usdc_weighted = checked_mul(e, values.usdc, TAKE_RATE_WEIGHT_USDC);
    let denominator = checked_add(
        e,
        checked_add(e, blnd_usdc_weighted, blnd_xlm_weighted),
        usdc_weighted,
    );
    if denominator == 0 {
        return TakeRateQuote {
            blnd_usdc: 0,
            blnd_xlm: 0,
            remainder: distribution,
            usdc: 0,
        };
    }

    let blnd_usdc = proportional_floor(e, distribution, blnd_usdc_weighted, denominator);
    let blnd_xlm = proportional_floor(e, distribution, blnd_xlm_weighted, denominator);
    let usdc = proportional_floor(e, distribution, usdc_weighted, denominator);
    let allocated = checked_add(e, checked_add(e, blnd_usdc, blnd_xlm), usdc);
    TakeRateQuote {
        blnd_usdc,
        blnd_xlm,
        remainder: checked_sub(e, distribution, allocated),
        usdc,
    }
}

/// Reserve the selected tier-token bid for one registered pool.
pub(crate) fn commit_interest_lot(
    e: &Env,
    pool: &Address,
    auction_id: &BytesN<32>,
    tier: BackstopTier,
    lot_value: i128,
) -> InterestLotQuote {
    require_registered_pool(e, pool);
    if get_interest_commitment(e, pool, tier).is_some()
        || has_interest_commitment_id(e, pool, auction_id)
    {
        panic_with_error!(e, BackstopError::InterestCommitmentExists);
    }
    if storage::get_pool_balance_for_tier(e, tier, pool).shares == 0 {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }

    let quote = build_interest_lot_quote(e, tier, lot_value);
    set_interest_commitment(
        e,
        pool,
        tier,
        &InterestCommitment {
            auction_id: auction_id.clone(),
            quote: quote.clone(),
        },
    );
    quote
}

/// Release one matching interest-auction commitment.
pub(crate) fn release_interest_lot(
    e: &Env,
    pool: &Address,
    tier: BackstopTier,
    auction_id: &BytesN<32>,
) {
    require_registered_pool(e, pool);
    let commitment = get_interest_commitment(e, pool, tier)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InterestCommitmentNotFound));
    if commitment.auction_id != *auction_id || commitment.quote.tier != tier {
        panic_with_error!(e, BackstopError::InterestCommitmentNotFound);
    }
    remove_interest_commitment(e, pool, tier);
}

/// Donate the time-scaled bid and resize or remove its commitment.
pub(crate) fn settle_interest_lot(
    e: &Env,
    pool: &Address,
    tier: BackstopTier,
    auction_id: &BytesN<32>,
    base_bid_amount: i128,
    bid_amount: i128,
    from: &Address,
) -> Option<InterestLotQuote> {
    require_registered_pool(e, pool);
    if base_bid_amount < 0
        || bid_amount < 0
        || bid_amount > base_bid_amount
        || from == pool
        || from == &e.current_contract_address()
    {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }

    let mut commitment = get_interest_commitment(e, pool, tier)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InterestCommitmentNotFound));
    if commitment.auction_id != *auction_id
        || commitment.quote.tier != tier
        || base_bid_amount > commitment.quote.bid_amount
    {
        panic_with_error!(e, BackstopError::InterestCommitmentNotFound);
    }

    if bid_amount > 0 {
        let token = TokenClient::new(e, &tier_token(e, tier));
        let backstop = e.current_contract_address();
        let from_before = token.balance(from);
        let backstop_before = token.balance(&backstop);
        token.transfer(from, &backstop, &bid_amount);
        if token.balance(from) != checked_sub(e, from_before, bid_amount)
            || token.balance(&backstop) != checked_add(e, backstop_before, bid_amount)
        {
            panic_with_error!(e, BackstopError::InvalidInterestLot);
        }
        apply_pool_tier_gain(e, tier, pool, bid_amount);
    }

    let previous_bid_amount = commitment.quote.bid_amount;
    let remaining_bid_amount = checked_sub(e, previous_bid_amount, base_bid_amount);
    if remaining_bid_amount == 0 {
        remove_interest_commitment(e, pool, tier);
        None
    } else {
        commitment.quote.lot_value = proportional_floor(
            e,
            commitment.quote.lot_value,
            remaining_bid_amount,
            previous_bid_amount,
        );
        commitment.quote.bid_amount = remaining_bid_amount;
        set_interest_commitment(e, pool, tier, &commitment);
        Some(commitment.quote)
    }
}

pub(crate) fn interest_tier_locked(e: &Env, tier: BackstopTier, pool: &Address) -> bool {
    get_interest_commitment(e, pool, tier).is_some()
}

fn build_take_rate_values(e: &Env, pool: &Address) -> TakeRateValues {
    TakeRateValues {
        blnd_usdc: quote_tier_value(e, BackstopTier::BlndUsdc, pool),
        blnd_xlm: quote_tier_value(e, BackstopTier::BlndXlm, pool),
        usdc: storage::get_pool_balance_for_tier(e, BackstopTier::Usdc, pool).tokens,
    }
}

fn quote_tier_value(e: &Env, tier: BackstopTier, pool: &Address) -> i128 {
    let amount = storage::get_pool_balance_for_tier(e, tier, pool).tokens;
    if amount < 0 {
        panic_with_error!(e, BackstopError::InvalidValuation);
    }
    if amount == 0 {
        return 0;
    }
    quote_lp_amount(e, tier, amount).usdc_value
}

fn build_interest_lot_quote(e: &Env, tier: BackstopTier, lot_value: i128) -> InterestLotQuote {
    if lot_value <= 0 {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }
    let bid_token = tier_token(e, tier);
    if TokenClient::new(e, &bid_token).decimals() != 7 {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }
    let (unit_value, valid_until) = if tier == BackstopTier::Usdc {
        (SCALAR_7, u64::MAX)
    } else {
        let quote = quote_lp_amount(e, tier, SCALAR_7);
        if quote.usdc_value <= 0 {
            panic_with_error!(e, BackstopError::InvalidValuation);
        }
        (quote.usdc_value, quote.valid_until)
    };
    let target_value = proportional_ceil(
        e,
        lot_value,
        INTEREST_LOT_PREMIUM_NUMERATOR,
        INTEREST_LOT_PREMIUM_DENOMINATOR,
    );
    InterestLotQuote {
        bid_token,
        bid_amount: proportional_ceil(e, target_value, SCALAR_7, unit_value),
        lot_value,
        tier,
        target_value,
        valid_until,
    }
}

fn apply_pool_tier_gain(e: &Env, tier: BackstopTier, pool: &Address, assets: i128) {
    if assets <= 0 {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }
    emissions::prepare_pool_weight_change(e, tier, pool);
    let mut balance = storage::get_pool_balance_for_tier(e, tier, pool);
    if balance.shares == 0 {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }
    balance.tokens = checked_add(e, balance.tokens, assets);
    storage::set_pool_balance_for_tier(e, tier, pool, &balance);
    emissions::finish_pool_weight_change(e, tier, pool);
}

fn get_interest_commitment(
    e: &Env,
    pool: &Address,
    tier: BackstopTier,
) -> Option<InterestCommitment> {
    e.storage()
        .temporary()
        .get(&InterestDataKey::InterestLot(pool.clone(), tier))
}

fn set_interest_commitment(
    e: &Env,
    pool: &Address,
    tier: BackstopTier,
    commitment: &InterestCommitment,
) {
    if commitment.quote.tier != tier {
        panic_with_error!(e, BackstopError::InvalidInterestLot);
    }
    let key = InterestDataKey::InterestLot(pool.clone(), tier);
    e.storage().temporary().set(&key, commitment);
    e.storage()
        .temporary()
        .extend_ttl(&key, AUCTION_TTL_THRESHOLD, AUCTION_TTL_BUMP);
}

fn remove_interest_commitment(e: &Env, pool: &Address, tier: BackstopTier) {
    e.storage()
        .temporary()
        .remove(&InterestDataKey::InterestLot(pool.clone(), tier));
}

fn has_interest_commitment_id(e: &Env, pool: &Address, auction_id: &BytesN<32>) -> bool {
    for tier in [
        BackstopTier::BlndUsdc,
        BackstopTier::BlndXlm,
        BackstopTier::Usdc,
    ] {
        if get_interest_commitment(e, pool, tier)
            .is_some_and(|commitment| commitment.auction_id == *auction_id)
        {
            return true;
        }
    }
    false
}

fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .filter(|result| *result >= 0)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_mul(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn proportional_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn proportional_ceil(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    let denominator = I256::from_i128(e, denominator);
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .add(&denominator)
        .sub(&I256::from_i32(e, 1))
        .div(&denominator)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(test)]
mod tests {
    use soroban_sdk::testutils::Address as _;

    use crate::{backstop::pool_bad_debt_commitment_count, testutils::create_backstop};

    use super::*;

    #[test]
    fn take_rate_quote_applies_canonical_weights() {
        let e = Env::default();
        let quote = quote_take_rate(
            &e,
            90,
            &TakeRateValues {
                blnd_usdc: 1,
                blnd_xlm: 1,
                usdc: 1,
            },
        );

        assert_eq!(
            quote,
            TakeRateQuote {
                blnd_usdc: 30,
                blnd_xlm: 40,
                remainder: 0,
                usdc: 20,
            }
        );
    }

    #[test]
    fn take_rate_quote_conserves_rounding_remainder() {
        let e = Env::default();
        let quote = quote_take_rate(
            &e,
            10,
            &TakeRateValues {
                blnd_usdc: 3,
                blnd_xlm: 2,
                usdc: 1,
            },
        );

        assert_eq!(
            quote.blnd_usdc + quote.blnd_xlm + quote.usdc + quote.remainder,
            10
        );
        assert_eq!(quote.remainder, 1);
    }

    #[test]
    fn interest_lot_premium_rounds_up_to_120_percent() {
        let e = Env::default();

        assert_eq!(proportional_ceil(&e, 10, 6, 5), 12);
        assert_eq!(proportional_ceil(&e, 11, 6, 5), 14);
    }

    #[test]
    fn interest_commitment_keys_are_isolated_by_tier_and_from_bad_debt() {
        let e = Env::default();
        let contract = create_backstop(&e);
        let pool = Address::generate(&e);
        let bid_token = Address::generate(&e);
        let blnd_usdc_commitment = InterestCommitment {
            auction_id: BytesN::from_array(&e, &[1; 32]),
            quote: InterestLotQuote {
                bid_token: bid_token.clone(),
                bid_amount: 12,
                lot_value: 10,
                tier: BackstopTier::BlndUsdc,
                target_value: 12,
                valid_until: u64::MAX,
            },
        };
        let blnd_xlm_commitment = InterestCommitment {
            auction_id: BytesN::from_array(&e, &[2; 32]),
            quote: InterestLotQuote {
                bid_token,
                bid_amount: 24,
                lot_value: 20,
                tier: BackstopTier::BlndXlm,
                target_value: 24,
                valid_until: u64::MAX,
            },
        };

        e.as_contract(&contract, || {
            set_interest_commitment(&e, &pool, BackstopTier::BlndUsdc, &blnd_usdc_commitment);
            set_interest_commitment(&e, &pool, BackstopTier::BlndXlm, &blnd_xlm_commitment);
            assert_eq!(
                get_interest_commitment(&e, &pool, BackstopTier::BlndUsdc),
                Some(blnd_usdc_commitment)
            );
            assert_eq!(
                get_interest_commitment(&e, &pool, BackstopTier::BlndXlm),
                Some(blnd_xlm_commitment)
            );
            assert!(interest_tier_locked(&e, BackstopTier::BlndUsdc, &pool));
            assert!(interest_tier_locked(&e, BackstopTier::BlndXlm, &pool));
            assert!(!interest_tier_locked(&e, BackstopTier::Usdc, &pool));
            assert_eq!(pool_bad_debt_commitment_count(&e, &pool), 0);
        });
    }
}
