use soroban_sdk::{contracttype, panic_with_error, Address, Env, Map, I256};

use crate::{errors::BackstopError, storage};

use super::{quote_lp_amount, require_registered_pool, BackstopTier};

const TAKE_RATE_WEIGHT_BLND_XLM: i128 = 4;
const TAKE_RATE_WEIGHT_BLND_USDC: i128 = 3;
const TAKE_RATE_WEIGHT_USDC: i128 = 2;
const MAX_TAKE_RATE_BATCH: u32 = 4;

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

#[cfg(test)]
mod tests {
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
}
