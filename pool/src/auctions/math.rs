use crate::{constants::SCALAR_7, errors::PoolError};
use soroban_sdk::{panic_with_error, Env, I256};

#[allow(clippy::zero_prefixed_literal)]
pub(crate) fn auction_modifiers(e: &Env, elapsed: u32) -> (i128, i128) {
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

pub(crate) fn proportional_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, PoolError::OverflowError);
    }
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

pub(crate) fn proportional_ceil(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, PoolError::OverflowError);
    }
    let denominator = I256::from_i128(e, denominator);
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .add(&denominator)
        .sub(&I256::from_i32(e, 1))
        .div(&denominator)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}
