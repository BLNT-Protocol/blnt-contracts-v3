use cast::{i128, u64};
use sep_41_token::TokenClient;
use soroban_sdk::{panic_with_error, Address, Env, Vec, I256};

use crate::{
    constants::{MAX_RESERVES, SCALAR_7},
    errors::PoolError,
    pool::User,
    storage::{self, ReserveEmissionCarry, ReserveEmissionData, UserEmissionData},
    validator::require_nonnegative,
};

const MAX_RESERVE_TOKEN_IDS: u32 = 2 * MAX_RESERVES;

/// Performs a claim against the given "reserve_token_ids" for "from"
pub fn execute_claim(e: &Env, from: &Address, reserve_token_ids: &Vec<u32>, to: &Address) -> i128 {
    if reserve_token_ids.is_empty() || reserve_token_ids.len() > MAX_RESERVE_TOKEN_IDS {
        panic_with_error!(e, PoolError::BadRequest);
    }
    let mut seen = Vec::new(e);
    for reserve_token_id in reserve_token_ids.iter() {
        if reserve_token_id >= MAX_RESERVE_TOKEN_IDS || seen.contains(reserve_token_id) {
            panic_with_error!(e, PoolError::BadRequest);
        }
        seen.push_back(reserve_token_id);
    }

    let from_state = User::load(e, from);
    let reserve_list = storage::get_res_list(e);
    let mut to_claim: i128 = 0;
    for reserve_token_id in reserve_token_ids.clone() {
        let reserve_index = reserve_token_id / 2;
        let reserve_addr = reserve_list.get(reserve_index);
        match reserve_addr {
            Some(res_address) => {
                let reserve_config = storage::get_res_config(e, &res_address);
                let reserve_data = storage::get_res_data(e, &res_address);
                let (user_balance, supply) = match reserve_token_id % 2 {
                    0 => (
                        from_state.get_liabilities(reserve_index),
                        reserve_data.d_supply,
                    ),
                    1 => (
                        from_state.get_total_supply(reserve_index),
                        reserve_data.b_supply,
                    ),
                    _ => panic_with_error!(e, PoolError::BadRequest),
                };
                to_claim = to_claim
                    .checked_add(claim_emissions(
                        e,
                        reserve_token_id,
                        supply,
                        10i128.pow(reserve_config.decimals),
                        from,
                        user_balance,
                    ))
                    .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            }
            None => {
                panic_with_error!(e, PoolError::BadRequest)
            }
        }
    }

    if to_claim > 0 {
        let backstop = storage::get_backstop(e);
        TokenClient::new(e, &storage::get_blnd_token(e)).transfer_from(
            &e.current_contract_address(),
            &backstop,
            to,
            &to_claim,
        );
    }
    to_claim
}

/// Update the emissions information about a reserve token. Must be called before any update
/// is made to the supply of debtTokens or blendTokens.
///
/// A reserve token id is a unique identifier for a position in a pool.
/// - For a reserve's dTokens (liabilities), reserve_token_id = reserve_index * 2
/// - For a reserve's bTokens (supply/collateral), reserve_token_id = reserve_index * 2 + 1
///
/// Returns the amount of tokens to claim, or zero if 'claim' is false
///
/// ### Arguments
/// * `res_token_id` - The reserve token id being acted against
/// * `supply` - The current supply of the reserve token
/// * `supply_scalar` - The scalar of the reserve token
/// * `user` - The user performing an action against the reserve
/// * `balance` - The current balance of the user
///
/// ### Panics
/// If the reserve update failed
pub fn update_emissions(
    e: &Env,
    res_token_id: u32,
    supply: i128,
    supply_scalar: i128,
    user: &Address,
    balance: i128,
) {
    if !storage::is_res_emis_configured(e, &res_token_id) {
        return;
    }
    if let Some(res_emis_data) = update_emission_data(e, res_token_id, supply, supply_scalar) {
        update_user_emissions(
            e,
            &res_emis_data,
            res_token_id,
            supply_scalar,
            user,
            balance,
            false,
        );
    }
}

/// Update and claim the emissions for a reserve token.
///
/// Returns the amount of tokens to claim.
///
/// ### Arguments
/// * `res_token_id` - The reserve token being acted against => (reserve index * 2 + (0 for debtToken or 1 for blendToken))
/// * `supply` - The current supply of the reserve token
/// * `supply_scalar` - The scalar of the reserve token
/// * `user` - The user claiming for the reserve
/// * `balance` - The current balance of the user
///
/// ### Panics
/// If the reserve update failed
fn claim_emissions(
    e: &Env,
    res_token_id: u32,
    supply: i128,
    supply_scalar: i128,
    user: &Address,
    balance: i128,
) -> i128 {
    if let Some(res_emis_data) = update_emission_data(e, res_token_id, supply, supply_scalar) {
        update_user_emissions(
            e,
            &res_emis_data,
            res_token_id,
            supply_scalar,
            user,
            balance,
            true,
        )
    } else {
        0
    }
}

/// Update the reserve token emission data
///
/// Returns the new ReserveEmissionData, if None if no data exists
///
/// ### Arguments
/// * `res_token_id` - The reserve token being acted against => (reserve index * 2 + (0 for debtToken or 1 for blendToken))
/// * `supply` - The current supply of the reserve token
/// * `supply_scalar` - The scalar of the reserve token
/// * `emis_config` - The reserve token emission configuration
///
/// ### Panics
/// If the reserve update failed
pub(super) fn update_emission_data(
    e: &Env,
    res_token_id: u32,
    supply: i128,
    supply_scalar: i128,
) -> Option<ReserveEmissionData> {
    match storage::get_res_emis_data(e, &res_token_id) {
        Some(mut res_emission_data) => {
            let mut carry = reserve_emission_carry(e, res_token_id, &res_emission_data);
            if res_emission_data.last_time >= res_emission_data.expiration
                || e.ledger().timestamp() == res_emission_data.last_time
                || (carry.remaining == 0 && carry.index_carry == 0)
                || supply == 0
            {
                storage::set_res_emis_carry(e, &res_token_id, &carry);
                return Some(res_emission_data);
            }

            let ledger_timestamp =
                core::cmp::min(e.ledger().timestamp(), res_emission_data.expiration);
            let elapsed = ledger_timestamp - res_emission_data.last_time;
            let seconds_left = res_emission_data.expiration - res_emission_data.last_time;
            let vested = if ledger_timestamp == res_emission_data.expiration {
                carry.remaining
            } else {
                mul_div_floor(e, carry.remaining, i128(elapsed), i128(seconds_left))
            };
            carry.remaining = checked_sub_nonnegative(e, carry.remaining, vested);

            let index_scale = supply_scalar
                .checked_mul(SCALAR_7)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            let numerator = I256::from_i128(e, vested)
                .mul(&I256::from_i128(e, index_scale))
                .add(&I256::from_i128(e, carry.index_carry));
            let denominator = I256::from_i128(e, supply);
            let additional_idx = numerator
                .div(&denominator)
                .to_i128()
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            carry.index_carry = numerator
                .sub(&I256::from_i128(e, additional_idx).mul(&denominator))
                .to_i128()
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));

            res_emission_data.index = res_emission_data
                .index
                .checked_add(additional_idx)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            res_emission_data.last_time = ledger_timestamp;
            res_emission_data.eps = remaining_eps(
                e,
                carry.remaining,
                res_emission_data.expiration - ledger_timestamp,
            );
            storage::set_res_emis_data(e, &res_token_id, &res_emission_data);
            storage::set_res_emis_carry(e, &res_token_id, &carry);
            Some(res_emission_data)
        }
        None => None, // no emission exists, no update is required
    }
}

fn update_user_emissions(
    e: &Env,
    res_emis_data: &ReserveEmissionData,
    res_token_id: u32,
    supply_scalar: i128,
    user: &Address,
    balance: i128,
    claim: bool,
) -> i128 {
    if let Some(user_data) = storage::get_user_emissions(e, user, &res_token_id) {
        if user_data.index != res_emis_data.index || claim {
            let mut accrual = user_data.accrued;
            let carry = storage::get_user_emission_carry(e, user, &res_token_id);
            let (to_accrue, carry) = user_accrual(
                e,
                balance,
                res_emis_data.index,
                user_data.index,
                supply_scalar,
                carry,
            );
            accrual = accrual
                .checked_add(to_accrue)
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
            return set_user_emissions(
                e,
                user,
                res_token_id,
                res_emis_data.index,
                accrual,
                carry,
                claim,
            );
        }
        0
    } else if balance == 0 {
        // first time the user registered an action with the asset since emissions were added
        set_user_emissions(e, user, res_token_id, res_emis_data.index, 0, 0, claim)
    } else {
        // user had tokens before emissions began, they are due any historical emissions
        let (to_accrue, carry) = user_accrual(e, balance, res_emis_data.index, 0, supply_scalar, 0);
        set_user_emissions(
            e,
            user,
            res_token_id,
            res_emis_data.index,
            to_accrue,
            carry,
            claim,
        )
    }
}

fn set_user_emissions(
    e: &Env,
    user: &Address,
    res_token_id: u32,
    index: i128,
    accrued: i128,
    carry: i128,
    claim: bool,
) -> i128 {
    if claim {
        storage::set_user_emissions(
            e,
            user,
            &res_token_id,
            &UserEmissionData { index, accrued: 0 },
        );
        storage::set_user_emission_carry(e, user, &res_token_id, carry);
        accrued
    } else {
        storage::set_user_emissions(e, user, &res_token_id, &UserEmissionData { index, accrued });
        storage::set_user_emission_carry(e, user, &res_token_id, carry);
        0
    }
}

fn reserve_emission_carry(
    e: &Env,
    res_token_id: u32,
    data: &ReserveEmissionData,
) -> ReserveEmissionCarry {
    storage::get_res_emis_carry(e, &res_token_id).unwrap_or_else(|| {
        let remaining = if data.expiration > data.last_time && data.eps > 0 {
            mul_div_floor(
                e,
                i128(data.eps),
                i128(data.expiration - data.last_time),
                SCALAR_7,
            )
        } else {
            0
        };
        ReserveEmissionCarry {
            index_carry: 0,
            remaining,
        }
    })
}

fn user_accrual(
    e: &Env,
    balance: i128,
    current_index: i128,
    prior_index: i128,
    supply_scalar: i128,
    carry: i128,
) -> (i128, i128) {
    if balance < 0 || supply_scalar <= 0 || carry < 0 {
        panic_with_error!(e, PoolError::BadRequest);
    }
    let index_delta = current_index
        .checked_sub(prior_index)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    require_nonnegative(e, &index_delta);
    let index_scale = supply_scalar
        .checked_mul(SCALAR_7)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let numerator = I256::from_i128(e, balance)
        .mul(&I256::from_i128(e, index_delta))
        .add(&I256::from_i128(e, carry));
    let scale = I256::from_i128(e, index_scale);
    let accrued = numerator
        .div(&scale)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let next_carry = numerator
        .sub(&I256::from_i128(e, accrued).mul(&scale))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    (accrued, next_carry)
}

fn mul_div_floor(e: &Env, left: i128, right: i128, denominator: i128) -> i128 {
    if left < 0 || right < 0 || denominator <= 0 {
        panic_with_error!(e, PoolError::BadRequest);
    }
    I256::from_i128(e, left)
        .mul(&I256::from_i128(e, right))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

fn checked_sub_nonnegative(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .filter(|value| *value >= 0)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

fn remaining_eps(e: &Env, remaining: i128, seconds: u64) -> u64 {
    if remaining == 0 || seconds == 0 {
        0
    } else {
        u64(mul_div_floor(e, remaining, SCALAR_7, i128(seconds)))
            .unwrap_or_else(|_| panic_with_error!(e, PoolError::OverflowError))
    }
}

#[cfg(test)]
mod tests {
    use crate::{pool::Positions, testutils};

    use super::*;
    use soroban_sdk::{
        map,
        testutils::{Address as AddressTestTrait, Ledger, LedgerInfo},
        unwrap::UnwrapOptimized,
        vec,
    };

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_execute_claim_rejects_empty_identifier_list() {
        let e = Env::default();
        let pool = testutils::create_pool(&e);
        let user = Address::generate(&e);
        e.as_contract(&pool, || {
            execute_claim(&e, &user, &vec![&e], &user);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_execute_claim_rejects_duplicate_identifiers() {
        let e = Env::default();
        let pool = testutils::create_pool(&e);
        let user = Address::generate(&e);
        e.as_contract(&pool, || {
            execute_claim(&e, &user, &vec![&e, 0_u32, 0_u32], &user);
        });
    }

    #[test]
    fn test_reserve_and_user_rounding_carry_forward() {
        let e = Env::default();
        let pool = testutils::create_pool(&e);
        let user = Address::generate(&e);
        e.ledger().set_timestamp(100);

        e.as_contract(&pool, || {
            storage::set_res_emis_data(
                &e,
                &0,
                &ReserveEmissionData {
                    expiration: 100,
                    eps: 1,
                    index: 0,
                    last_time: 0,
                },
            );
            storage::set_res_emis_carry(
                &e,
                &0,
                &ReserveEmissionCarry {
                    index_carry: 0,
                    remaining: 1,
                },
            );
            update_emission_data(&e, 0, 2 * SCALAR_7, 1);
            let reserve_carry = storage::get_res_emis_carry(&e, &0).unwrap();
            assert_eq!(reserve_carry.remaining, 0);
            assert_eq!(reserve_carry.index_carry, SCALAR_7);

            update_user_emissions(
                &e,
                &ReserveEmissionData {
                    expiration: 100,
                    eps: 0,
                    index: 1,
                    last_time: 100,
                },
                0,
                1,
                &user,
                1,
                false,
            );
            assert_eq!(storage::get_user_emission_carry(&e, &user, &0), 1);

            update_user_emissions(
                &e,
                &ReserveEmissionData {
                    expiration: 100,
                    eps: 0,
                    index: SCALAR_7,
                    last_time: 100,
                },
                0,
                1,
                &user,
                1,
                false,
            );
            assert_eq!(
                storage::get_user_emissions(&e, &user, &0).unwrap().accrued,
                1
            );
            assert_eq!(storage::get_user_emission_carry(&e, &user, &0), 0);
        });
    }

    /********** update_emissions **********/

    #[test]
    fn test_update_emissions() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply: i128 = 50_0000000;
        let user_position: i128 = 2_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 23456780000000,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 12345670000000,
                accrued: 0_1000000,
            };
            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            update_emissions(
                &e,
                res_token_index,
                supply,
                1_0000000,
                &samwise,
                user_position,
            );

            let new_reserve_emission_data =
                storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_reserve_emission_data.last_time, 1501000000);
            assert_eq!(
                new_user_emission_data.index,
                new_reserve_emission_data.index
            );
            assert_eq!(new_user_emission_data.accrued, 400_3222222);
        });
    }

    #[test]
    fn test_update_emissions_no_data_ignores() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply: i128 = 100_0000000;
        let user_position: i128 = 2_0000000;
        e.as_contract(&pool, || {
            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;

            update_emissions(
                &e,
                res_token_index,
                supply,
                1_0000000,
                &samwise,
                user_position,
            );

            assert!(storage::get_res_emis_data(&e, &res_token_index).is_none());
            assert!(storage::get_user_emissions(&e, &samwise, &res_token_index).is_none());
        });
    }

    #[test]
    #[should_panic(expected = "attempt to subtract with overflow")]
    fn test_update_emissions_negative_time_diff() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply: i128 = 50_0000000;
        let user_position: i128 = 2_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 2345678,
                last_time: 1501000000 + 1,
            };
            let user_emission_data = UserEmissionData {
                index: 1234567,
                accrued: 0_1000000,
            };
            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            update_emissions(
                &e,
                res_token_index,
                supply,
                1_0000000,
                &samwise,
                user_position,
            );
        });
    }

    #[test]
    fn test_update_emission_no_overflow() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1510000000, // 10^7 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        // Supply of 1 trillion at 18 decimals
        let supply: i128 = 1000000000000_000000000000000000;
        let user_position: i128 = 1_000000000000000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 100_00000000000000,
                index: 23456780000000,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 12345670000000,
                accrued: 0_1000000,
            };
            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            // Intermediate index math should not overflow using SorobanFixedPoint
            // 10^7 * 10^16 * 10^18 = 10^41 > i128::MAX
            update_emissions(
                &e,
                res_token_index,
                supply,
                1_000000000000000000,
                &samwise,
                user_position,
            );

            let new_reserve_emission_data =
                storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_reserve_emission_data.last_time, 1510000000);
            assert_eq!(
                new_user_emission_data.index,
                new_reserve_emission_data.index
            );
            assert_eq!(new_user_emission_data.accrued, 2121111);
        });
    }

    /********** claim_emissions **********/

    #[test]
    fn test_claim_emissions() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply: i128 = 50_0000000;
        let user_position: i128 = 2_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 23456780000000,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 12345670000000,
                accrued: 0_1000000,
            };
            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            let result = claim_emissions(
                &e,
                res_token_index,
                supply,
                1_0000000,
                &samwise,
                user_position,
            );

            assert_eq!(result, 400_3222222);
            let new_reserve_emission_data =
                storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_reserve_emission_data.last_time, 1501000000);
            assert_eq!(
                new_user_emission_data.index,
                new_reserve_emission_data.index
            );
            assert_eq!(new_user_emission_data.accrued, 0);
        });
    }

    #[test]
    fn test_claim_emissions_no_config_ignores() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply: i128 = 100_0000000;
        let user_position: i128 = 2_0000000;
        e.as_contract(&pool, || {
            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;

            claim_emissions(
                &e,
                res_token_index,
                supply,
                1_0000000,
                &samwise,
                user_position,
            );

            assert!(storage::get_res_emis_data(&e, &res_token_index).is_none());
            assert!(storage::get_user_emissions(&e, &samwise, &res_token_index).is_none());
        });
    }

    /********** update emission data **********/

    #[test]
    fn test_update_emission_data_no_config_returns_none() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 50_0000000;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;

            // no emission information stored

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    assert!(false)
                }
                None => {
                    assert!(storage::get_res_emis_data(&e, &res_token_index).is_none());
                }
            }
        });
    }

    #[test]
    fn test_update_emission_data_expired_returns_old() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1601000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 50_0000000;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 2345678,
                last_time: 1600000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    let new_reserve_emission_data =
                        storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
                    assert_eq!(
                        new_reserve_emission_data.last_time,
                        reserve_emission_data.last_time
                    );
                    assert_eq!(new_reserve_emission_data.index, reserve_emission_data.index);
                }
                None => assert!(false),
            }
        });
    }

    #[test]
    fn test_update_emission_data_updated_this_block_returns_old() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 50_0000000;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 2345678,
                last_time: 1501000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    let new_reserve_emission_data =
                        storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
                    assert_eq!(
                        new_reserve_emission_data.last_time,
                        reserve_emission_data.last_time
                    );
                    assert_eq!(new_reserve_emission_data.index, reserve_emission_data.index);
                }
                None => assert!(false),
            }
        });
    }

    #[test]
    fn test_update_emission_data_no_eps_returns_old() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 50_0000000;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0,
                index: 2345678,
                last_time: 1500000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    let new_reserve_emission_data =
                        storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
                    assert_eq!(
                        new_reserve_emission_data.last_time,
                        reserve_emission_data.last_time
                    );
                    assert_eq!(new_reserve_emission_data.index, reserve_emission_data.index);
                }
                None => assert!(false),
            }
        });
    }

    #[test]
    fn test_update_emission_data_no_supply_returns_old() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 0;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 2345678,
                last_time: 1500000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    let new_reserve_emission_data =
                        storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
                    assert_eq!(
                        new_reserve_emission_data.last_time,
                        reserve_emission_data.last_time
                    );
                    assert_eq!(new_reserve_emission_data.index, reserve_emission_data.index);
                }
                None => assert!(false),
            }
        });
    }

    #[test]
    fn test_update_emission_data_past_exp() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1700000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 100_0000000;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000001,
                eps: 0_01000000000000,
                index: 1234567890000000,
                last_time: 1500000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    let new_reserve_emission_data =
                        storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
                    assert_eq!(new_reserve_emission_data.last_time, 1600000001);
                    assert_eq!(new_reserve_emission_data.index, 10012_34577890000000);
                }
                None => assert!(false),
            }
        });
    }

    #[test]
    fn test_update_emission_data_rounds_down() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000005,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply = 100_0001111;
        let supply_scalar = 1_0000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 1234567890000000,
                last_time: 1500000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;

            storage::set_res_emis_data(&e, &res_token_index, &reserve_emission_data);

            let result = update_emission_data(&e, res_token_index, supply, supply_scalar);
            match result {
                Some(_) => {
                    let new_reserve_emission_data =
                        storage::get_res_emis_data(&e, &res_token_index).unwrap_optimized();
                    assert_eq!(new_reserve_emission_data.last_time, 1500000005);
                    assert_eq!(new_reserve_emission_data.index, 1234617889944450);
                }
                None => assert!(false),
            }
        });
    }

    /********** update_user_emissions **********/

    #[test]
    fn test_update_user_emissions_first_time() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 123456789,
                last_time: 1500000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;
            update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                false,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, 0);
        });
    }

    #[test]
    fn test_update_user_emissions_first_time_had_tokens() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0_5000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 1234567890000000,
                last_time: 1500000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;
            update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                false,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, 6_1728394);
        });
    }

    #[test]
    fn test_update_user_emissions_no_bal_no_accrual() {
        let e = Env::default();
        e.mock_all_auths();
        let pool = testutils::create_pool(&e);

        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 123456789,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 56789,
                accrued: 0_1000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                false,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, 0_1000000);
        });
    }

    #[test]
    fn test_update_user_emissions_if_accrued_skips() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0_5000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 123456789,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 123456789,
                accrued: 1_1000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                false,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, user_emission_data.accrued);
        });
    }

    #[test]
    fn test_update_user_emissions_accrues() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);
        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0_5000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 1234567890000000,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 567890000000,
                accrued: 0_1000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                false,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, 6_2700000);
        });
    }

    #[test]
    fn test_update_user_emissions_claim_returns_accrual() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0_5000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 1234567890000000,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 567890000000,
                accrued: 0_1000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            let result = update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                true,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, 0);
            assert_eq!(result, 6_2700000);
        });
    }

    #[test]
    fn test_update_user_emissions_claim_first_time_claims_tokens() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0_5000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 1234567890000000,
                last_time: 1500000000,
            };

            let res_token_type = 0;
            let res_token_index = 1 * 2 + res_token_type;
            let result = update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                true,
            );

            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index).unwrap_optimized();
            assert_eq!(new_user_emission_data.index, reserve_emission_data.index);
            assert_eq!(new_user_emission_data.accrued, 0);
            assert_eq!(result, 6_1728394);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_update_user_emissions_negative_index() {
        let e = Env::default();
        e.mock_all_auths();

        let pool = testutils::create_pool(&e);

        let samwise = Address::generate(&e);

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let supply_scalar = 1_0000000;
        let user_balance = 0_5000000;
        e.as_contract(&pool, || {
            let reserve_emission_data = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 123456789,
                last_time: 1500000000,
            };
            let user_emission_data = UserEmissionData {
                index: 123456789 + 1,
                accrued: 0_1000000,
            };

            let res_token_type = 1;
            let res_token_index = 1 * 2 + res_token_type;
            storage::set_user_emissions(&e, &samwise, &res_token_index, &user_emission_data);

            update_user_emissions(
                &e,
                &reserve_emission_data,
                res_token_index,
                supply_scalar,
                &samwise,
                user_balance,
                true,
            );
        });
    }

    //********** execute claim **********//

    #[test]
    fn test_execute_claim() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let merry = Address::generate(&e);

        let (_blnd, blnd_token_client) = testutils::create_blnd_token(&e, &pool, &bombadil);
        let backstop = Address::generate(&e);
        blnd_token_client.mint(&backstop, &100_000_0000000);
        blnd_token_client.approve(&backstop, &pool, &100_000_0000000, &1_000_000);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.decimals = 5;
        reserve_data.b_supply = 100_00000;
        reserve_data.d_supply = 50_00000;
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.decimals = 9;
        reserve_config.index = 1;
        reserve_data.b_supply = 100_000_000_000;
        reserve_data.d_supply = 50_000_000_000;
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        let user_positions = Positions {
            liabilities: map![&e, (0, 2_00000)],
            collateral: map![&e, (1, 1_000_000_000)],
            supply: map![&e, (1, 1_000_000_000)],
        };
        e.as_contract(&pool, || {
            storage::set_backstop(&e, &backstop);
            storage::set_user_positions(&e, &samwise, &user_positions);

            let reserve_emission_data_0 = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 23456780000000,
                last_time: 1500000000,
            };
            let user_emission_data_0 = UserEmissionData {
                index: 12345670000000,
                accrued: 0_1000000,
            };
            let res_token_index_0 = 0; // d_token for reserve 0

            let reserve_emission_data_1 = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01500000000000,
                index: 13456780000000,
                last_time: 1500000000,
            };
            let user_emission_data_1 = UserEmissionData {
                index: 12345670000000,
                accrued: 1_0000000,
            };
            let res_token_index_1 = 1 * 2 + 1; // b_token for reserve 1

            storage::set_res_emis_data(&e, &res_token_index_0, &reserve_emission_data_0);
            storage::set_user_emissions(&e, &samwise, &res_token_index_0, &user_emission_data_0);

            storage::set_res_emis_data(&e, &res_token_index_1, &reserve_emission_data_1);
            storage::set_user_emissions(&e, &samwise, &res_token_index_1, &user_emission_data_1);

            let reserve_token_ids: Vec<u32> = vec![&e, res_token_index_0, res_token_index_1];
            let result = execute_claim(&e, &samwise, &reserve_token_ids, &merry);

            let new_reserve_emission_data =
                storage::get_res_emis_data(&e, &res_token_index_0).unwrap_optimized();
            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index_0).unwrap_optimized();
            assert_eq!(new_reserve_emission_data.last_time, 1501000000);
            assert_eq!(
                new_user_emission_data.index,
                new_reserve_emission_data.index
            );
            assert_eq!(new_user_emission_data.accrued, 0);

            let new_reserve_emission_data_1 =
                storage::get_res_emis_data(&e, &res_token_index_1).unwrap_optimized();
            let new_user_emission_data_1 =
                storage::get_user_emissions(&e, &samwise, &res_token_index_1).unwrap_optimized();
            assert_eq!(new_reserve_emission_data_1.last_time, 1501000000);
            assert_eq!(
                new_user_emission_data_1.index,
                new_reserve_emission_data_1.index
            );
            assert_eq!(new_user_emission_data.accrued, 0);
            assert_eq!(result, 400_3222222 + 301_0222222);

            // verify tokens are sent
            assert_eq!(blnd_token_client.balance(&merry), 400_3222222 + 301_0222222);
            assert_eq!(
                blnd_token_client.balance(&backstop),
                100_000_0000000 - (400_3222222 + 301_0222222)
            )
        });
    }

    #[test]
    fn test_execute_claim_with_already_claimed_reserve() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let merry = Address::generate(&e);

        let (_blnd, blnd_token_client) = testutils::create_blnd_token(&e, &pool, &bombadil);
        let backstop = Address::generate(&e);
        blnd_token_client.mint(&backstop, &100_000_0000000);
        blnd_token_client.approve(&backstop, &pool, &100_000_0000000, &1_000_000);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.decimals = 5;
        reserve_data.b_supply = 100_00000;
        reserve_data.d_supply = 50_00000;
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.decimals = 9;
        reserve_config.index = 1;
        reserve_data.b_supply = 100_000_000_000;
        reserve_data.d_supply = 50_000_000_000;
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        let user_positions = Positions {
            liabilities: map![&e, (0, 2_00000)],
            collateral: map![&e, (1, 1_000_000_000)],
            supply: map![&e, (1, 1_000_000_000)],
        };
        e.as_contract(&pool, || {
            storage::set_backstop(&e, &backstop);
            storage::set_user_positions(&e, &samwise, &user_positions);

            let reserve_emission_data_0 = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 23456780000000,
                last_time: 1500000000,
            };
            let user_emission_data_0 = UserEmissionData {
                index: 12345670000000,
                accrued: 0_1000000,
            };
            let res_token_index_0 = 0; // d_token for reserve 0

            let reserve_emission_data_1 = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01500000000000,
                index: 13456780000000,
                last_time: 1501000000,
            };
            let user_emission_data_1 = UserEmissionData {
                index: 13456780000000,
                accrued: 0,
            };
            let res_token_index_1 = 1 * 2 + 1; // b_token for reserve 1

            storage::set_res_emis_data(&e, &res_token_index_0, &reserve_emission_data_0);
            storage::set_user_emissions(&e, &samwise, &res_token_index_0, &user_emission_data_0);

            storage::set_res_emis_data(&e, &res_token_index_1, &reserve_emission_data_1);
            storage::set_user_emissions(&e, &samwise, &res_token_index_1, &user_emission_data_1);

            let reserve_token_ids: Vec<u32> = vec![&e, res_token_index_0, res_token_index_1];
            let result = execute_claim(&e, &samwise, &reserve_token_ids, &merry);

            let new_reserve_emission_data =
                storage::get_res_emis_data(&e, &res_token_index_0).unwrap_optimized();
            let new_user_emission_data =
                storage::get_user_emissions(&e, &samwise, &res_token_index_0).unwrap_optimized();
            assert_eq!(new_reserve_emission_data.last_time, 1501000000);
            assert_eq!(
                new_user_emission_data.index,
                new_reserve_emission_data.index
            );
            assert_eq!(new_user_emission_data.accrued, 0);

            let new_reserve_emission_data_1 =
                storage::get_res_emis_data(&e, &res_token_index_1).unwrap_optimized();
            let new_user_emission_data_1 =
                storage::get_user_emissions(&e, &samwise, &res_token_index_1).unwrap_optimized();
            assert_eq!(new_reserve_emission_data_1.last_time, 1501000000);
            assert_eq!(
                new_user_emission_data_1.index,
                new_reserve_emission_data_1.index
            );
            assert_eq!(new_user_emission_data.accrued, 0);
            assert_eq!(result, 400_3222222);

            // verify tokens are sent
            assert_eq!(blnd_token_client.balance(&merry), 400_3222222);
            assert_eq!(
                blnd_token_client.balance(&backstop),
                100_000_0000000 - 400_3222222
            )
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_calc_claim_with_invalid_reserve_panics() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let merry = Address::generate(&e);
        let (blnd, blnd_token_client) = testutils::create_blnd_token(&e, &pool, &bombadil);
        let (usdc, _) = testutils::create_token_contract(&e, &bombadil);
        let (backstop_token, _) = testutils::create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);

        let (backstop, _) = testutils::create_backstop(&e, &pool, &backstop_token, &usdc, &blnd);
        // mock backstop having emissions for pool
        e.as_contract(&backstop, || {
            blnd_token_client.approve(&backstop, &pool, &100_000_0000000_i128, &1000000);
        });
        blnd_token_client.mint(&backstop, &100_000_0000000);

        e.ledger().set(LedgerInfo {
            timestamp: 1501000000, // 10^6 seconds have passed
            protocol_version: 27,
            sequence_number: 123,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.decimals = 5;
        reserve_data.b_supply = 100_00000;
        reserve_data.d_supply = 50_00000;
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (mut reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_config.decimals = 9;
        reserve_config.index = 1;
        reserve_data.b_supply = 100_000_000_000;
        reserve_data.d_supply = 50_000_000_000;
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        let user_positions = Positions {
            liabilities: map![&e, (0, 2_00000)],
            collateral: map![&e, (1, 1_000_000_000)],
            supply: map![&e, (1, 1_000_000_000)],
        };
        e.as_contract(&pool, || {
            storage::set_backstop(&e, &backstop);
            storage::set_user_positions(&e, &samwise, &user_positions);

            let reserve_emission_data_0 = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01000000000000,
                index: 2345678,
                last_time: 1500000000,
            };
            let user_emission_data_0 = UserEmissionData {
                index: 1234567,
                accrued: 0_1000000,
            };
            let res_token_index_0 = 0; // d_token for reserve 0

            let reserve_emission_data_1 = ReserveEmissionData {
                expiration: 1600000000,
                eps: 0_01500000000000,
                index: 1345678,
                last_time: 1500000000,
            };
            let user_emission_data_1 = UserEmissionData {
                index: 1234567,
                accrued: 1_0000000,
            };
            let res_token_index_1 = 1 * 2 + 1; // b_token for reserve 1

            storage::set_res_emis_data(&e, &res_token_index_0, &reserve_emission_data_0);
            storage::set_user_emissions(&e, &samwise, &res_token_index_0, &user_emission_data_0);

            storage::set_res_emis_data(&e, &res_token_index_1, &reserve_emission_data_1);
            storage::set_user_emissions(&e, &samwise, &res_token_index_1, &user_emission_data_1);

            let reserve_token_ids: Vec<u32> = vec![&e, res_token_index_0, res_token_index_1, 6];
            execute_claim(&e, &samwise, &reserve_token_ids, &merry);

            assert_eq!(blnd_token_client.balance(&backstop), 100_000_0000000)
        });
    }
}
