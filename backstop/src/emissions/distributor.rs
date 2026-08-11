//! V2-style backstop emission indexes generalized by pool and tier.

use soroban_sdk::{panic_with_error, Address, Env, I256};

use crate::{
    backstop::{BackstopTier, PoolBalance, UserBalance},
    constants::{SCALAR_14, SCALAR_7},
    storage::{self, BackstopEmissionData, UserEmissionData},
    BackstopError,
};

pub(super) const STREAM_SECONDS: u64 = 7 * 24 * 60 * 60;

pub(crate) fn checkpoint_user_emissions(
    e: &Env,
    tier: BackstopTier,
    pool: &Address,
    user: &Address,
) -> UserEmissionData {
    let pool_balance = storage::get_pool_balance_for_tier(e, tier, pool);
    let user_balance = storage::get_user_balance_for_tier(e, tier, pool, user);
    let Some(emission_data) = checkpoint_emission_data(e, tier, pool, &pool_balance) else {
        return storage::get_user_emis_data(e, tier, pool, user).unwrap_or(empty_user_data());
    };
    let user_data = accrue_user_emissions(
        e,
        storage::get_user_emis_data(e, tier, pool, user).unwrap_or(empty_user_data()),
        &user_balance,
        emission_data.index,
    );
    storage::set_user_emis_data(e, tier, pool, user, &user_data);
    user_data
}

#[cfg(test)]
pub(crate) fn preview_user_emissions(
    e: &Env,
    tier: BackstopTier,
    pool: &Address,
    user: &Address,
) -> UserEmissionData {
    let pool_balance = storage::get_pool_balance_for_tier(e, tier, pool);
    let user_balance = storage::get_user_balance_for_tier(e, tier, pool, user);
    let current_index = storage::get_backstop_emis_data(e, tier, pool)
        .map(|data| advance_emission_data(e, data, &pool_balance).index)
        .unwrap_or(0);
    accrue_user_emissions(
        e,
        storage::get_user_emis_data(e, tier, pool, user).unwrap_or(empty_user_data()),
        &user_balance,
        current_index,
    )
}

pub(crate) fn claim_emissions(e: &Env, tier: BackstopTier, pool: &Address, user: &Address) -> i128 {
    let mut user_data = checkpoint_user_emissions(e, tier, pool, user);
    let accrued = user_data.accrued;
    if accrued > 0 {
        user_data.accrued = 0;
        storage::set_user_emis_data(e, tier, pool, user, &user_data);
    }
    accrued
}

pub(crate) fn set_emission_eps(e: &Env, tier: BackstopTier, pool: &Address, pending: i128) {
    if pending < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let now = e.ledger().timestamp();
    let pool_balance = storage::get_pool_balance_for_tier(e, tier, pool);
    let existing = checkpoint_emission_data(e, tier, pool, &pool_balance);
    if pending == 0 {
        return;
    }
    let mut data = existing.unwrap_or(BackstopEmissionData {
        eps: 0,
        expiration: 0,
        index: 0,
        index_carry: 0,
        last_time: now,
        schedule_carry: 0,
    });
    let remaining_seconds = data.expiration.saturating_sub(now);
    let scaled_total = I256::from_i128(e, pending)
        .mul(&I256::from_i128(e, SCALAR_7))
        .add(
            &I256::from_i128(e, i128::from(remaining_seconds))
                .mul(&I256::from_i128(e, i128::from(data.eps))),
        )
        .add(&I256::from_i128(e, data.schedule_carry));
    let duration = I256::from_i128(e, i128::from(STREAM_SECONDS));
    let eps = scaled_total
        .div(&duration)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    data.schedule_carry = scaled_total
        .sub(&I256::from_i128(e, eps).mul(&duration))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    data.eps =
        u64::try_from(eps).unwrap_or_else(|_| panic_with_error!(e, BackstopError::OverflowError));
    data.expiration = now
        .checked_add(STREAM_SECONDS)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    data.last_time = now;
    validate_emission_data(e, &data);
    storage::set_backstop_emis_data(e, tier, pool, &data);
}

fn checkpoint_emission_data(
    e: &Env,
    tier: BackstopTier,
    pool: &Address,
    pool_balance: &PoolBalance,
) -> Option<BackstopEmissionData> {
    let data = storage::get_backstop_emis_data(e, tier, pool)?;
    let data = advance_emission_data(e, data, pool_balance);
    storage::set_backstop_emis_data(e, tier, pool, &data);
    Some(data)
}

fn advance_emission_data(
    e: &Env,
    mut data: BackstopEmissionData,
    pool_balance: &PoolBalance,
) -> BackstopEmissionData {
    validate_emission_data(e, &data);
    let now = e.ledger().timestamp();
    if data.last_time > now || pool_balance.shares < pool_balance.q4w {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let stream_end = now.min(data.expiration);
    let active_shares = pool_balance.shares - pool_balance.q4w;
    if active_shares > 0 && stream_end > data.last_time {
        let emitted_scaled = I256::from_i128(e, i128::from(stream_end - data.last_time))
            .mul(&I256::from_i128(e, i128::from(data.eps)))
            .add(&if stream_end == data.expiration {
                I256::from_i128(e, data.schedule_carry)
            } else {
                I256::from_i128(e, 0)
            });
        let numerator = emitted_scaled
            .mul(&I256::from_i128(e, SCALAR_7))
            .add(&I256::from_i128(e, data.index_carry));
        let denominator = I256::from_i128(e, active_shares);
        let increment = numerator
            .div(&denominator)
            .to_i128()
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        data.index_carry = numerator
            .sub(&I256::from_i128(e, increment).mul(&denominator))
            .to_i128()
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        data.index = checked_add(e, data.index, increment);
    }
    if stream_end == data.expiration && stream_end > data.last_time {
        data.schedule_carry = 0;
    }
    data.last_time = now;
    data
}

fn accrue_user_emissions(
    e: &Env,
    mut data: UserEmissionData,
    balance: &UserBalance,
    current_index: i128,
) -> UserEmissionData {
    if balance.shares < 0 || current_index < data.index {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let numerator = I256::from_i128(e, balance.shares)
        .mul(&I256::from_i128(e, current_index - data.index))
        .add(&I256::from_i128(e, data.carry));
    let scale = I256::from_i128(e, SCALAR_14);
    let accrued = numerator
        .div(&scale)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    data.carry = numerator
        .sub(&I256::from_i128(e, accrued).mul(&scale))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    data.accrued = checked_add(e, data.accrued, accrued);
    data.index = current_index;
    validate_user_data(e, &data);
    data
}

fn empty_user_data() -> UserEmissionData {
    UserEmissionData {
        accrued: 0,
        carry: 0,
        index: 0,
    }
}

fn validate_emission_data(e: &Env, data: &BackstopEmissionData) {
    if data.index < 0
        || data.index_carry < 0
        || data.schedule_carry < 0
        || data.schedule_carry >= i128::from(STREAM_SECONDS)
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn validate_user_data(e: &Env, data: &UserEmissionData) {
    if data.accrued < 0 || data.carry < 0 || data.carry >= SCALAR_14 || data.index < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}
