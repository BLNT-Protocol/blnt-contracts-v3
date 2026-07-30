use crate::{
    constants::{MAX_RESERVES, SCALAR_7},
    dependencies::BackstopClient,
    errors::PoolError,
    events::PoolEvents,
    storage::{self, ReserveConfig, ReserveEmissionCarry, ReserveEmissionData},
};
use cast::{i128, u64};
use soroban_sdk::{
    contracttype, map, panic_with_error, unwrap::UnwrapOptimized, Address, Env, Map, Vec, I256,
};

use super::distributor;

const MAX_RESERVE_TOKEN_IDS: u32 = 2 * MAX_RESERVES;
const EMISSION_STREAM_SECONDS: u64 = 7 * 24 * 60 * 60;

// Types

/// Metadata for a pool's reserve emission configuration
#[contracttype]
pub struct ReserveEmissionMetadata {
    pub res_index: u32,
    pub res_type: u32,
    pub share: u64,
}

/// Set the pool emissions
///
/// These will not be applied until the next `update_emissions` is run
///
/// ### Arguments
/// * `res_emission_metadata` - A vector of `ReserveEmissionMetadata` that details each reserve token's share
///                             if the total pool eps
///
/// ### Panics
/// If any res_emission_metadata is included where share is 0, the reserve index is invalid,
/// or the reserve type is invalid
pub fn set_pool_emissions(e: &Env, res_emission_metadata: Vec<ReserveEmissionMetadata>) {
    if res_emission_metadata.len() > MAX_RESERVE_TOKEN_IDS {
        panic_with_error!(e, PoolError::BadRequest);
    }
    let mut pool_emissions: Map<u32, u64> = map![e];

    let reserve_list = storage::get_res_list(e);
    for metadata in res_emission_metadata {
        if metadata.res_type > 1
            || reserve_list.get(metadata.res_index).is_none()
            || metadata.share == 0
        {
            panic_with_error!(e, PoolError::BadRequest);
        }
        let key = metadata
            .res_index
            .checked_mul(2)
            .and_then(|value| value.checked_add(metadata.res_type))
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        if pool_emissions.contains_key(key) {
            panic_with_error!(e, PoolError::BadRequest);
        }
        pool_emissions.set(key, metadata.share);
    }

    storage::set_pool_emissions(e, &pool_emissions);
}

/// Consume emitted tokens from the backstop and distribute them to reserves
///
/// Returns the number of new tokens distributed for emissions
///
/// ### Panics
/// If the pool is not in the backstop reward zone
pub fn gulp_emissions(e: &Env) -> i128 {
    require_enabled_pool_emissions(e);
    let backstop = storage::get_backstop(e);
    let new_emissions =
        BackstopClient::new(e, &backstop).gulp_pool_emissions(&e.current_contract_address());
    do_gulp_emissions(e, new_emissions);
    new_emissions
}

/// Validate the pool's current emission targets before reserving its tranche
/// from the backstop. This prevents an empty or fully disabled configuration
/// from making otherwise claimable BLND unreachable.
fn require_enabled_pool_emissions(e: &Env) {
    let pool_emissions = storage::get_pool_emissions(e);
    if pool_emissions.is_empty() {
        panic_with_error!(e, PoolError::BadRequest);
    }

    let reserve_list = storage::get_res_list(e);
    for (res_token_id, _) in pool_emissions.iter() {
        let reserve_index = res_token_id / 2;
        let res_asset_address = reserve_list.get_unchecked(reserve_index);
        if storage::get_res_config(e, &res_asset_address).enabled {
            return;
        }
    }
    panic_with_error!(e, PoolError::BadRequest);
}

fn do_gulp_emissions(e: &Env, new_emissions: i128) {
    // ensure enough tokens are being emitted to avoid rounding issues
    if new_emissions < SCALAR_7 {
        panic_with_error!(e, PoolError::BadRequest)
    }
    let pool_emissions = storage::get_pool_emissions(e);
    let reserve_list = storage::get_res_list(e);
    let mut pool_emis_enabled: Vec<(ReserveConfig, Address, u32, u64)> = Vec::new(e);

    let mut total_share: i128 = 0;
    for (res_token_id, res_eps_share) in pool_emissions.iter() {
        let reserve_index = res_token_id / 2;
        let res_asset_address = reserve_list.get_unchecked(reserve_index);
        let res_config = storage::get_res_config(e, &res_asset_address);

        if res_config.enabled {
            pool_emis_enabled.push_back((
                res_config,
                res_asset_address,
                res_token_id,
                res_eps_share,
            ));
            total_share = total_share
                .checked_add(i128(res_eps_share))
                .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        }
    }
    let distribution = new_emissions
        .checked_add(storage::get_pool_emission_carry(e))
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let mut allocated = 0_i128;
    for (res_config, res_asset_address, res_token_id, res_eps_share) in pool_emis_enabled {
        let new_reserve_emissions =
            mul_div_floor(e, distribution, i128(res_eps_share), total_share);
        allocated = allocated
            .checked_add(new_reserve_emissions)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));

        update_reserve_emission_eps(
            e,
            &res_config,
            &res_asset_address,
            res_token_id,
            new_reserve_emissions,
        );
    }
    let carry = distribution
        .checked_sub(allocated)
        .filter(|carry| *carry >= 0)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    storage::set_pool_emission_carry(e, carry);
}

fn update_reserve_emission_eps(
    e: &Env,
    reserve_config: &ReserveConfig,
    asset: &Address,
    res_token_id: u32,
    new_reserve_emissions: i128,
) {
    let reserve_data = storage::get_res_data(e, asset);
    let supply = match res_token_id % 2 {
        0 => reserve_data.d_supply,
        1 => reserve_data.b_supply,
        _ => panic_with_error!(e, PoolError::BadRequest),
    };
    let now = e.ledger().timestamp();
    let expiration = now
        .checked_add(EMISSION_STREAM_SECONDS)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));

    if let Some(mut emission_data) = distributor::update_emission_data(
        e,
        res_token_id,
        supply,
        10i128.pow(reserve_config.decimals),
    ) {
        let mut carry = storage::get_res_emis_carry(e, &res_token_id)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::BadRequest));
        carry.remaining = carry
            .remaining
            .checked_add(new_reserve_emissions)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        let eps = stream_eps(e, carry.remaining, EMISSION_STREAM_SECONDS);

        emission_data.expiration = expiration;
        emission_data.eps = eps;
        emission_data.last_time = now;
        storage::set_res_emis_data(e, &res_token_id, &emission_data);
        storage::set_res_emis_carry(e, &res_token_id, &carry);
        PoolEvents::reserve_emission_update(e, res_token_id, eps, expiration);
    } else {
        // no config or data exists yet - first time this reserve token will get emission
        let eps = stream_eps(e, new_reserve_emissions, EMISSION_STREAM_SECONDS);
        storage::set_res_emis_data(
            e,
            &res_token_id,
            &ReserveEmissionData {
                expiration,
                eps,
                index: 0,
                last_time: now,
            },
        );
        storage::set_res_emis_carry(
            e,
            &res_token_id,
            &ReserveEmissionCarry {
                index_carry: 0,
                remaining: new_reserve_emissions,
            },
        );
        PoolEvents::reserve_emission_update(e, res_token_id, eps, expiration);
    }
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

fn stream_eps(e: &Env, remaining: i128, seconds: u64) -> u64 {
    u64(mul_div_floor(e, remaining, SCALAR_7, i128(seconds))).unwrap_optimized()
}

#[cfg(test)]
mod tests {
    use crate::testutils;

    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        unwrap::UnwrapOptimized,
        vec, Address,
    };

    /********** gulp_emissions ********/

    #[test]
    fn test_gulp_emissions_no_pool_emissions_does_nothing() {
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let new_emissions: i128 = 302_400_0000000;
        let pool_emissions: Map<u32, u64> = map![&e];

        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);

            do_gulp_emissions(&e, new_emissions);

            assert!(storage::get_res_emis_data(&e, &0).is_none());
            assert!(storage::get_res_emis_data(&e, &1).is_none());
            assert!(storage::get_res_emis_data(&e, &2).is_none());
            assert!(storage::get_res_emis_data(&e, &3).is_none());
        });
    }

    #[test]
    fn test_gulp_emissions_carries_pool_rounding_remainder() {
        let e = Env::default();
        e.mock_all_auths();
        let pool = testutils::create_pool(&e);
        let admin = Address::generate(&e);
        let (reserve_config, reserve_data) = testutils::default_reserve_meta();

        for _ in 0..3 {
            let (underlying, _) = testutils::create_token_contract(&e, &admin);
            testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);
        }

        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &map![&e, (1, 1_u64), (3, 1_u64), (5, 1_u64)]);

            do_gulp_emissions(&e, SCALAR_7 + 1);
            assert_eq!(storage::get_pool_emission_carry(&e), 2);

            do_gulp_emissions(&e, SCALAR_7 + 1);
            assert_eq!(storage::get_pool_emission_carry(&e), 1);
            for reserve_token_id in [1_u32, 3, 5] {
                assert_eq!(
                    storage::get_res_emis_carry(&e, &reserve_token_id)
                        .unwrap()
                        .remaining,
                    6_666_667
                );
            }
        });
    }

    #[test]
    fn test_gulp_emissions() {
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let new_emissions: i128 = 302_400_0000000;
        let pool_emissions: Map<u32, u64> = map![
            &e,
            (0, 0_2000000), // reserve_0 liability
            (2, 0_5500000), // reserve_1 liability
            (3, 0_2500000)  // reserve_1 supply
        ];

        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.last_time = 1499900000;
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);
        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_2, &reserve_config, &reserve_data);

        // setup reserve_0 liability to have emissions remaining
        let old_r_0_l_data = ReserveEmissionData {
            eps: 0_15000000000000,
            expiration: 1500000200,
            index: 999990000000,
            last_time: 1499980000,
        };

        // setup reserve_1 liability to have no emissions

        // steup reserve_1 supply to have emissions expired
        let old_r_1_s_data = ReserveEmissionData {
            eps: 0_35000000000000,
            expiration: 1499990000,
            index: 111110000000,
            last_time: 1499990000,
        };
        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);
            storage::set_res_emis_data(&e, &0, &old_r_0_l_data);
            storage::set_res_emis_data(&e, &3, &old_r_1_s_data);

            do_gulp_emissions(&e, new_emissions);

            assert!(storage::get_res_emis_data(&e, &1).is_none());
            assert!(storage::get_res_emis_data(&e, &4).is_none());
            assert!(storage::get_res_emis_data(&e, &5).is_none());

            // verify reserve_0 liability leftover emissions were carried over
            let r_0_l_config = storage::get_res_emis_data(&e, &0).unwrap_optimized();
            let r_0_l_data = storage::get_res_emis_data(&e, &0).unwrap_optimized();
            assert_eq!(r_0_l_config.expiration, 1500000000 + 7 * 24 * 60 * 60);
            assert_eq!(r_0_l_config.eps, 0_10004960317460);
            assert_eq!(r_0_l_data.index, (99999 + 40 * SCALAR_7) * SCALAR_7);
            assert_eq!(r_0_l_data.last_time, 1500000000);

            // verify reserve_1 liability initialized emissions
            let r_1_l_config = storage::get_res_emis_data(&e, &2).unwrap_optimized();
            let r_1_l_data = storage::get_res_emis_data(&e, &2).unwrap_optimized();
            assert_eq!(r_1_l_config.expiration, 1500000000 + 7 * 24 * 60 * 60);
            assert_eq!(r_1_l_config.eps, 0_27500000000000);
            assert_eq!(r_1_l_data.index, 0);
            assert_eq!(r_1_l_data.last_time, 1500000000);

            // verify reserve_1 supply updated reserve data to the correct timestamp
            let r_1_s_config = storage::get_res_emis_data(&e, &3).unwrap_optimized();
            let r_1_s_data = storage::get_res_emis_data(&e, &3).unwrap_optimized();
            assert_eq!(r_1_s_config.expiration, 1500000000 + 7 * 24 * 60 * 60);
            assert_eq!(r_1_s_config.eps, 0_12500000000000);
            assert_eq!(r_1_s_data.index, 111110000000);
            assert_eq!(r_1_s_data.last_time, 1500000000);
        });
    }

    #[test]
    fn test_gulp_emissions_when_a_reserve_disabled() {
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let new_emissions: i128 = 302_400_0000000;
        let pool_emissions: Map<u32, u64> = map![
            &e,
            (0, 0_2000000), // reserve_0 liability
            (2, 0_5500000), // reserve_1 liability
            (3, 0_2500000), // reserve_1 supply
            (4, 0_1000000), // reserve_2 liability
            (5, 0_1000000), // reserve_2 supply
        ];

        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.last_time = 1499900000;
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        let mut reserve_config_disabled = reserve_config.clone();
        reserve_config_disabled.enabled = false;
        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(
            &e,
            &pool,
            &underlying_2,
            &reserve_config_disabled,
            &reserve_data,
        );

        // setup reserve_0 liability to have emissions remaining
        let old_r_0_l_data = ReserveEmissionData {
            eps: 0_15000000000000,
            expiration: 1500000200,
            index: 999990000000,
            last_time: 1499980000,
        };

        // setup reserve_1 liability to have no emissions

        // steup reserve_1 supply to have emissions expired
        let old_r_1_s_data = ReserveEmissionData {
            eps: 0_35000000000000,
            expiration: 1499990000,
            index: 111110000000,
            last_time: 1499990000,
        };
        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);
            storage::set_res_emis_data(&e, &0, &old_r_0_l_data);
            storage::set_res_emis_data(&e, &3, &old_r_1_s_data);

            do_gulp_emissions(&e, new_emissions);

            assert!(storage::get_res_emis_data(&e, &1).is_none());
            assert!(storage::get_res_emis_data(&e, &4).is_none());
            assert!(storage::get_res_emis_data(&e, &5).is_none());

            // verify reserve_0 liability leftover emissions were carried over
            let r_0_l_config = storage::get_res_emis_data(&e, &0).unwrap_optimized();
            let r_0_l_data = storage::get_res_emis_data(&e, &0).unwrap_optimized();
            assert_eq!(r_0_l_config.expiration, 1500000000 + 7 * 24 * 60 * 60);
            assert_eq!(r_0_l_config.eps, 0_10004960317460);
            assert_eq!(r_0_l_data.index, (99999 + 40 * SCALAR_7) * SCALAR_7);
            assert_eq!(r_0_l_data.last_time, 1500000000);

            // verify reserve_1 liability initialized emissions
            let r_1_l_config = storage::get_res_emis_data(&e, &2).unwrap_optimized();
            let r_1_l_data = storage::get_res_emis_data(&e, &2).unwrap_optimized();
            assert_eq!(r_1_l_config.expiration, 1500000000 + 7 * 24 * 60 * 60);
            assert_eq!(r_1_l_config.eps, 0_27500000000000);
            assert_eq!(r_1_l_data.index, 0);
            assert_eq!(r_1_l_data.last_time, 1500000000);

            // verify reserve_1 supply updated reserve data to the correct timestamp
            let r_1_s_config = storage::get_res_emis_data(&e, &3).unwrap_optimized();
            let r_1_s_data = storage::get_res_emis_data(&e, &3).unwrap_optimized();
            assert_eq!(r_1_s_config.expiration, 1500000000 + 7 * 24 * 60 * 60);
            assert_eq!(r_1_s_config.eps, 0_12500000000000);
            assert_eq!(r_1_s_data.index, 111110000000);
            assert_eq!(r_1_s_data.last_time, 1500000000);

            // verify reserve_2 liability is None
            let r_2_l_config = storage::get_res_emis_data(&e, &4);
            let r_2_l_data = storage::get_res_emis_data(&e, &4);
            assert!(r_2_l_config.is_none());
            assert!(r_2_l_data.is_none());

            // verify reserve_2 supply is None
            let r_2_s_config = storage::get_res_emis_data(&e, &5);
            let r_2_s_data = storage::get_res_emis_data(&e, &5);
            assert!(r_2_s_config.is_none());
            assert!(r_2_s_data.is_none());
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_gulp_emissions_too_small() {
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let new_emissions: i128 = 1000000;
        let pool_emissions: Map<u32, u64> = map![
            &e,
            (0, 0_2000000), // reserve_0 liability
            (2, 0_5500000), // reserve_1 liability
            (3, 0_2500000)  // reserve_1 supply
        ];

        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.last_time = 1499900000;
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);
        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_2, &reserve_config, &reserve_data);

        // setup reserve_0 liability to have emissions remaining
        let old_r_0_l_data = ReserveEmissionData {
            eps: 0_1500000,
            expiration: 1500000200,
            index: 99999,
            last_time: 1499980000,
        };

        // setup reserve_1 liability to have no emissions

        // steup reserve_1 supply to have emissions expired
        let old_r_1_s_data = ReserveEmissionData {
            eps: 0_3500000,
            expiration: 1499990000,
            index: 11111,
            last_time: 1499990000,
        };
        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);
            storage::set_res_emis_data(&e, &0, &old_r_0_l_data);
            storage::set_res_emis_data(&e, &3, &old_r_1_s_data);

            do_gulp_emissions(&e, new_emissions);
        });
    }

    /********** set_pool_emissions **********/

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_set_pool_emissions_rejects_more_than_sixty_entries() {
        let e = Env::default();
        let pool = testutils::create_pool(&e);
        let mut metadata = Vec::new(&e);
        for _ in 0..=MAX_RESERVE_TOKEN_IDS {
            metadata.push_back(ReserveEmissionMetadata {
                res_index: 0,
                res_type: 0,
                share: 1,
            });
        }

        e.as_contract(&pool, || set_pool_emissions(&e, metadata));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_set_pool_emissions_rejects_duplicate_entries() {
        let e = Env::default();
        let pool = testutils::create_pool(&e);
        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        let admin = Address::generate(&e);
        let (underlying, _) = testutils::create_token_contract(&e, &admin);
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        e.as_contract(&pool, || {
            set_pool_emissions(
                &e,
                vec![
                    &e,
                    ReserveEmissionMetadata {
                        res_index: 0,
                        res_type: 1,
                        share: 1,
                    },
                    ReserveEmissionMetadata {
                        res_index: 0,
                        res_type: 1,
                        share: 2,
                    },
                ],
            )
        });
    }

    #[test]
    fn test_set_pool_emissions() {
        let e = Env::default();
        e.mock_all_auths();
        e.cost_estimate().budget().reset_unlimited();

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);
        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_2, &reserve_config, &reserve_data);
        let (underlying_3, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_3, &reserve_config, &reserve_data);

        let pool_emissions: Map<u32, u64> = map![&e, (2, 0_7500000),];
        let res_emission_metadata: Vec<ReserveEmissionMetadata> = vec![
            &e,
            ReserveEmissionMetadata {
                res_index: 0,
                res_type: 1,
                share: 0_3500000,
            },
            ReserveEmissionMetadata {
                res_index: 3,
                res_type: 0,
                share: 0_6500000,
            },
        ];

        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);

            set_pool_emissions(&e, res_emission_metadata);

            let new_pool_emissions = storage::get_pool_emissions(&e);
            assert_eq!(new_pool_emissions.len(), 2);
            assert_eq!(new_pool_emissions.get(1).unwrap_optimized(), 0_3500000);
            assert_eq!(new_pool_emissions.get(6).unwrap_optimized(), 0_6500000);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_set_pool_emissions_panics_if_anyone_share_equal_0() {
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);
        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_2, &reserve_config, &reserve_data);
        let (underlying_3, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_3, &reserve_config, &reserve_data);

        let pool_emissions: Map<u32, u64> = map![&e, (2, 0_7500000),];
        let res_emission_metadata: Vec<ReserveEmissionMetadata> = vec![
            &e,
            ReserveEmissionMetadata {
                res_index: 0,
                res_type: 1,
                share: 0_3500000,
            },
            ReserveEmissionMetadata {
                res_index: 3,
                res_type: 0,
                share: 0_6500001,
            },
            ReserveEmissionMetadata {
                res_index: 3,
                res_type: 1,
                share: 0,
            },
        ];

        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);

            set_pool_emissions(&e, res_emission_metadata);
        });
    }

    #[test]
    fn test_set_pool_emissions_ok_if_under_100() {
        let e = Env::default();
        e.mock_all_auths();
        e.cost_estimate().budget().reset_unlimited();

        e.ledger().set(LedgerInfo {
            timestamp: 1500000000,
            protocol_version: 27,
            sequence_number: 20100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = testutils::create_pool(&e);
        let bombadil = Address::generate(&e);

        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);
        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);
        let (underlying_2, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_2, &reserve_config, &reserve_data);
        let (underlying_3, _) = testutils::create_token_contract(&e, &bombadil);
        testutils::create_reserve(&e, &pool, &underlying_3, &reserve_config, &reserve_data);

        let pool_emissions: Map<u32, u64> = map![&e, (2, 0_7500000),];
        let res_emission_metadata: Vec<ReserveEmissionMetadata> = vec![
            &e,
            ReserveEmissionMetadata {
                res_index: 0,
                res_type: 1,
                share: 0_3400000,
            },
            ReserveEmissionMetadata {
                res_index: 3,
                res_type: 0,
                share: 0_6500000,
            },
        ];

        e.as_contract(&pool, || {
            storage::set_pool_emissions(&e, &pool_emissions);

            set_pool_emissions(&e, res_emission_metadata);

            let new_pool_emissions = storage::get_pool_emissions(&e);
            assert_eq!(new_pool_emissions.len(), 2);
            assert_eq!(new_pool_emissions.get(1).unwrap_optimized(), 0_3400000);
            assert_eq!(new_pool_emissions.get(6).unwrap_optimized(), 0_6500000);
        });
    }
}
