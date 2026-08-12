//! V2-style backstop emission indexes generalized by pool and tier.

use soroban_sdk::{panic_with_error, Address, Env, I256};

use crate::{
    backstop::{BackstopTier, PoolBalance, UserBalance},
    constants::{SCALAR_14, SCALAR_7},
    storage::{self, BackstopEmissionData, UserEmissionData},
    BackstopError,
};

pub(super) const STREAM_SECONDS: u64 = 7 * 24 * 60 * 60;

pub fn update_emissions(
    e: &Env,
    tier: BackstopTier,
    pool: &Address,
    user: &Address,
) -> UserEmissionData {
    let pool_balance = storage::get_pool_balance_for_tier(e, tier, pool);
    let user_balance = storage::get_user_balance_for_tier(e, tier, pool, user);
    let Some(emission_data) = update_emission_data(e, tier, pool, &pool_balance) else {
        return storage::get_user_emis_data(e, tier, pool, user).unwrap_or(empty_user_data());
    };
    let user_data = update_user_emissions(
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
    update_user_emissions(
        e,
        storage::get_user_emis_data(e, tier, pool, user).unwrap_or(empty_user_data()),
        &user_balance,
        current_index,
    )
}

pub(crate) fn claim_emissions(e: &Env, tier: BackstopTier, pool: &Address, user: &Address) -> i128 {
    let mut user_data = update_emissions(e, tier, pool, user);
    let accrued = user_data.accrued;
    if accrued > 0 {
        user_data.accrued = 0;
        storage::set_user_emis_data(e, tier, pool, user, &user_data);
    }
    accrued
}

pub(crate) fn set_backstop_emission_eps(
    e: &Env,
    tier: BackstopTier,
    pool: &Address,
    pending: i128,
) {
    if pending < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let now = e.ledger().timestamp();
    let pool_balance = storage::get_pool_balance_for_tier(e, tier, pool);
    let existing = update_emission_data(e, tier, pool, &pool_balance);
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

fn update_emission_data(
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

fn update_user_emissions(
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
#[cfg(test)]
mod tests {
    use crate::{backstop::BackstopTier, testutils::create_backstop, Q4W};

    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        unwrap::UnwrapOptimized,
        vec,
    };

    /********** update_emissions **********/

    #[test]
    fn test_update_emissions() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            let user_balance = UserBalance {
                shares: 9_0000000,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1)
                    .unwrap_optimized();
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise)
                    .unwrap_optimized();
            assert_eq!(new_backstop_data.last_time, block_timestamp);
            assert_eq!(new_backstop_data.index, 82488886666666);
            assert_eq!(new_user_data.accrued, 7_4140001);
            assert_eq!(new_user_data.index, 82488886666666);
        });
    }

    #[test]
    fn test_update_emissions_no_data() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        e.as_contract(&backstop_id, || {
            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            let user_balance = UserBalance {
                shares: 9_0000000,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1);
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);
            assert!(new_backstop_data.is_none());
            assert!(new_user_data.is_none());
        });
    }

    #[test]
    fn test_update_emissions_first_action() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_04200000000000,
            index: 222220000000,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            let user_balance = UserBalance {
                shares: 0,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1)
                    .unwrap_optimized();
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise)
                    .unwrap_optimized();
            assert_eq!(new_backstop_data.last_time, block_timestamp);
            assert_eq!(new_backstop_data.index, 345882220000000);
            assert_eq!(new_user_data.accrued, 0);
            assert_eq!(new_user_data.index, 345882220000000);
        });
    }

    #[test]
    fn test_update_emissions_config_set_after_user() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 12345;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_04200000000000,
            index: 0,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            let user_balance = UserBalance {
                shares: 9_0000000,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1)
                    .unwrap_optimized();
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise)
                    .unwrap_optimized();
            assert_eq!(new_backstop_data.last_time, block_timestamp);
            assert_eq!(new_backstop_data.index, 345660000000000);
            assert_eq!(new_user_data.accrued, 31_1094000);
            assert_eq!(new_user_data.index, 345660000000000);
        });
    }

    #[test]
    fn test_update_emissions_q4w_not_counted() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 4_5000000,
            };
            let q4w: Q4W = Q4W {
                amount: (4_5000000),
                exp: (5000),
            };
            let user_balance = UserBalance {
                shares: 4_5000000,
                q4w: vec![&e, q4w],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1)
                    .unwrap_optimized();
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise)
                    .unwrap_optimized();
            assert_eq!(new_backstop_data.last_time, block_timestamp);
            assert_eq!(new_backstop_data.index, 85033216563573);
            assert_eq!(new_user_data.accrued, 38214950);
            assert_eq!(new_user_data.index, 85033216563573);
        });
    }

    #[test]
    fn test_update_emissions_fully_q4w_emissions_lost() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 150_0000000,
            };
            let q4w: Q4W = Q4W {
                amount: (150_0000000),
                exp: (5000),
            };
            let user_balance = UserBalance {
                shares: 4_5000000,
                q4w: vec![&e, q4w],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1)
                    .unwrap_optimized();
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise)
                    .unwrap_optimized();
            assert_eq!(new_backstop_data.last_time, block_timestamp);
            assert_eq!(new_backstop_data.index, backstop_emissions_data.index);
            assert_eq!(new_user_data.accrued, 50002);
            assert_eq!(new_user_data.index, backstop_emissions_data.index);
        });
    }

    #[test]
    fn test_claim_emissions() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 111110000000,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            let user_balance = UserBalance {
                shares: 9_0000000,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            let result = claim_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);

            let new_backstop_data =
                storage::get_backstop_emis_data(&e, BackstopTier::BlndUsdc, &pool_1)
                    .unwrap_optimized();
            let new_user_data =
                storage::get_user_emis_data(&e, BackstopTier::BlndUsdc, &pool_1, &samwise)
                    .unwrap_optimized();
            assert_eq!(result, 7_4140001);
            assert_eq!(new_backstop_data.last_time, block_timestamp);
            assert_eq!(new_backstop_data.index, 82488886666666);
            assert_eq!(new_user_data.accrued, 0);
            assert_eq!(new_user_data.index, 82488886666666);
        });
    }

    // @dev: The below tests should be impossible states to reach, but are left
    //       in to ensure any bad state does not result in incorrect emissions.

    #[test]
    #[should_panic(expected = "Error(Contract, #1027)")]
    fn test_update_emissions_more_q4w_than_shares_panics() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 22222,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 11111,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 150_0000001,
            };
            let q4w: Q4W = Q4W {
                amount: (4_5000000),
                exp: (5000),
            };
            let user_balance = UserBalance {
                shares: 4_5000000,
                q4w: vec![&e, q4w],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1027)")]
    fn test_update_emissions_negative_time_dif() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 22222,
            last_time: block_timestamp + 1,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 11111,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            let user_balance = UserBalance {
                shares: 4_5000000,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1027)")]
    fn test_update_emissions_negative_user_index() {
        let e = Env::default();
        let block_timestamp = 1713139200 + 1234;
        e.ledger().set(LedgerInfo {
            timestamp: block_timestamp,
            protocol_version: 27,
            sequence_number: 0,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_id = create_backstop(&e);
        let pool_1 = Address::generate(&e);
        let samwise = Address::generate(&e);

        let backstop_emissions_data = BackstopEmissionData {
            expiration: 1713139200 + 7 * 24 * 60 * 60,
            eps: 0_10000000000000,
            index: 222220000000,
            last_time: 1713139200,
            index_carry: 0,
            schedule_carry: 0,
        };
        let user_emissions_data = UserEmissionData {
            index: 345660000000000 + 1,
            accrued: 3,
            carry: 0,
        };
        e.as_contract(&backstop_id, || {
            storage::set_backstop_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &backstop_emissions_data,
            );
            storage::set_user_emis_data(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_emissions_data,
            );

            let pool_balance = PoolBalance {
                shares: 150_0000000,
                tokens: 200_0000000,
                q4w: 0,
            };
            let user_balance = UserBalance {
                shares: 4_5000000,
                q4w: vec![&e],
            };

            storage::set_pool_balance_for_tier(&e, BackstopTier::BlndUsdc, &pool_1, &pool_balance);
            storage::set_user_balance_for_tier(
                &e,
                BackstopTier::BlndUsdc,
                &pool_1,
                &samwise,
                &user_balance,
            );
            update_emissions(&e, BackstopTier::BlndUsdc, &pool_1, &samwise);
        });
    }
}
