use soroban_sdk::{panic_with_error, Address, Env};

use crate::{events::PoolEvents, storage, AuctionType, PoolError};

use super::{Pool, User};

/// Handles any bad debt that exists for "user"
pub fn bad_debt(e: &Env, user: &Address) {
    let backstop = storage::get_backstop(e);
    if user == &backstop {
        crate::auctions::default_backstop_bad_debt(e);
    } else {
        let mut pool = Pool::load(e);
        let mut user_state = User::load(e, user);
        if storage::has_auction(e, &(AuctionType::UserLiquidation as u32), user) {
            panic_with_error!(e, PoolError::AuctionInProgress);
        }
        let had_bad_debt = check_and_handle_user_bad_debt(e, &mut pool, user, &mut user_state);

        if had_bad_debt {
            user_state.store(e);
            pool.store_cached_reserves(e);
        } else {
            panic_with_error!(e, PoolError::BadRequest);
        }
    }
}

/// Check if a user has bad debt.
///
/// If they do, pass the bad debt off to the backstop.
///
/// If not, this function does nothing.
///
/// `user_state` is modified in place, and is not stored to chain. If this function
/// is invoked, `user_state` must be written to chain afterwards.
///
/// `pool` is modified in place, and reserve updates are not stored to chain. If this function
/// is invoked, `pool.store_cached_reserves()` must be called afterwards.
///
/// ### Arguments
/// * pool - The pool
/// * user - The user's address
/// * user_state - The user's state
///
/// ### Returns
/// * `true` if the user's bad debt was handled, `false` otherwise
pub fn check_and_handle_user_bad_debt(
    e: &Env,
    pool: &mut Pool,
    user: &Address,
    user_state: &mut User,
) -> bool {
    if user_state.has_liabilities() {
        remove_zero_value_collateral(e, pool, user, user_state);
    }
    if user_state.has_liabilities() && !user_state.has_collateral() {
        // no more collateral left to liquidate for this user
        // pass the rest of the debt to the backstop as bad debt
        let reserve_list = storage::get_res_list(e);
        let backstop_address = storage::get_backstop(e);
        let mut backstop_state = User::load(e, &backstop_address);
        for (reserve_index, liability_balance) in user_state.positions.liabilities.iter() {
            let asset = reserve_list.get_unchecked(reserve_index);
            let mut reserve = pool.load_reserve(e, &asset, true);
            backstop_state.add_liabilities(e, &mut reserve, liability_balance);
            user_state.remove_liabilities(e, &mut reserve, liability_balance);
            pool.cache_reserve(reserve);

            PoolEvents::bad_debt(e, user.clone(), asset, liability_balance);
        }
        backstop_state.store(e);
        return true;
    }
    return false;
}

/// Forfeit and remove collateral shares whose live reserve exchange rate gives
/// them zero underlying value. This prevents a nonempty, economically empty
/// collateral map from blocking the ordinary bad-debt handoff or recovering
/// value for a borrower whose debt was socialized. Supply-only holders keep
/// their recovery claim if later borrower interest raises the reserve's b_rate.
fn remove_zero_value_collateral(e: &Env, pool: &mut Pool, user: &Address, user_state: &mut User) {
    let reserve_list = storage::get_res_list(e);
    for (reserve_index, b_tokens) in user_state.positions.collateral.iter() {
        let asset = reserve_list.get_unchecked(reserve_index);
        let mut reserve = pool.load_reserve(e, &asset, true);
        if reserve.to_asset_from_b_token(e, b_tokens) == 0 {
            user_state.remove_collateral(e, &mut reserve, b_tokens);
            pool.cache_reserve(reserve);
            PoolEvents::withdraw_collateral(e, asset, user.clone(), 0, b_tokens);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auctions::AuctionData,
        storage::PoolConfig,
        testutils::{
            self, create_backstop, create_blnt_token, create_comet_lp_pool, create_pool,
            create_token_contract,
        },
        Positions,
    };
    use soroban_sdk::{
        map,
        testutils::{Address as _, Ledger, LedgerInfo},
        vec, Address,
    };

    /***** bad_debt *****/

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_bad_debt_user_panics_no_change() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_500_0000000; // over 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 50_987_654_321)],
            collateral: map![&e, (0, 100_1234567)],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &samwise, &positions);

            bad_debt(&e, &samwise);
        });
    }

    #[test]
    fn test_bad_debt_user() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (backstop_address, backstop_client) =
            create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_500_0000000; // over 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data_0);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data_1);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 50_987_654_321)],
            collateral: map![&e],
            supply: map![&e, (0, 100_1234567)],
        };
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 0_5000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);

            bad_debt(&e, &samwise);

            // assert user forgiven liabilities and assigned to backstop
            let post_positions = storage::get_user_positions(&e, &samwise);
            assert_eq!(post_positions.liabilities.len(), 0);
            assert_eq!(post_positions.collateral.len(), 0);
            assert_eq!(post_positions.supply, positions.supply);

            let post_backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(
                post_backstop_positions.liabilities,
                map![&e, (0, 0_5000000 + 1_5000000), (1, 50_987_654_321)]
            );
            assert_eq!(post_backstop_positions.collateral.len(), 0);
            assert_eq!(post_backstop_positions.supply.len(), 0);

            // assert pool reserves updated
            let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
            assert_eq!(post_reserve_data_0.last_time, 100);
            assert_eq!(post_reserve_data_0.d_supply, reserve_data_0.d_supply);
            assert!(post_reserve_data_0.d_rate > reserve_data_0.d_rate);
            assert_eq!(post_reserve_data_0.b_supply, reserve_data_0.b_supply);
            assert!(post_reserve_data_0.b_rate > reserve_data_0.b_rate);
            let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
            assert_eq!(post_reserve_data_1.last_time, 100);
            assert_eq!(post_reserve_data_1.d_supply, reserve_data_1.d_supply);
            assert!(post_reserve_data_1.d_rate > reserve_data_1.d_rate);
            assert_eq!(post_reserve_data_0.b_supply, reserve_data_0.b_supply);
            assert!(post_reserve_data_0.b_rate > reserve_data_0.b_rate);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1212)")]
    fn test_bad_debt_user_with_ongoing_auction() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (backstop_address, backstop_client) =
            create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_500_0000000; // over 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data_0);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data_1);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 50_987_654_321)],
            collateral: map![&e],
            supply: map![&e, (0, 100_1234567)],
        };
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 0_5000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        let auction = AuctionData {
            bid: map![&e],
            block: 0,
            lot: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);
            storage::set_auction(
                &e,
                &(AuctionType::UserLiquidation as u32),
                &samwise,
                &auction,
            );

            bad_debt(&e, &samwise);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_bad_debt_backstop_no_change() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (backstop_address, backstop_client) =
            create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_500_0000000; // over 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data_0);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data_1);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 3_5000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);

            bad_debt(&e, &backstop_address);
        });
    }

    #[test]
    fn test_bad_debt_backstop_defaults_after_verified_tier_exhaustion() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let (blnt, _) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, _) = create_token_contract(&e, &bombadil);
        let (lp_token, _) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (backstop_address, _) = create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data_0);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data_1);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 3_5000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);

            bad_debt(&e, &backstop_address);

            // assert backstop forgiven liabilities
            let post_backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(post_backstop_positions.liabilities.len(), 0);
            assert_eq!(
                post_backstop_positions.collateral,
                backstop_positions.collateral
            );
            assert_eq!(post_backstop_positions.supply, backstop_positions.supply);

            // assert pool reserves updated
            let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
            assert_eq!(post_reserve_data_0.last_time, 100);
            assert!(post_reserve_data_0.d_supply < reserve_data_0.d_supply);
            assert!(post_reserve_data_0.d_rate > reserve_data_0.d_rate);
            assert_eq!(post_reserve_data_0.b_supply, reserve_data_0.b_supply);
            assert!(post_reserve_data_0.b_rate < reserve_data_0.b_rate);
            let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
            assert_eq!(post_reserve_data_1.last_time, 100);
            assert!(post_reserve_data_1.d_supply < reserve_data_1.d_supply);
            assert!(post_reserve_data_1.d_rate > reserve_data_1.d_rate);
            assert_eq!(post_reserve_data_1.b_supply, reserve_data_1.b_supply);
            assert!(post_reserve_data_1.b_rate < reserve_data_1.b_rate);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1212)")]
    fn test_bad_debt_backstop_ongoing_auction_rejects_default() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (backstop_address, backstop_client) =
            create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_000_0000000; // under 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data_0);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data_1);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 3_5000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        let auction = AuctionData {
            bid: map![&e],
            block: 0,
            lot: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);
            storage::set_auction(
                &e,
                &(AuctionType::BadDebtAuction as u32),
                &backstop_address,
                &auction,
            );

            bad_debt(&e, &backstop_address);
        });
    }

    /***** check_and_handle_user_bad_debt *****/

    #[test]
    fn test_check_and_handle_user_bad_debt_with_collateral() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_500_0000000; // over 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 50_987_654_321)],
            collateral: map![&e, (0, 100_1234567)],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &samwise, &positions);

            let mut pool = Pool::load(&e);
            let mut user = User::load(&e, &samwise);

            let result = check_and_handle_user_bad_debt(&e, &mut pool, &samwise, &mut user);
            assert_eq!(result, false);

            // assert user not modified
            assert_eq!(user.positions.liabilities, positions.liabilities);
            assert_eq!(user.positions.collateral, positions.collateral);
            assert_eq!(user.positions.supply, positions.supply);

            // Positive-value collateral remains unchanged and does not need
            // to be cached for storage.
            assert_eq!(pool.reserves.len(), 0);
        });
    }

    #[test]
    fn test_check_and_handle_user_bad_debt_removes_zero_value_collateral() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let pool = create_pool(&e);
        let admin = Address::generate(&e);
        let supplier = Address::generate(&e);
        let depositor = Address::generate(&e);
        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &admin);
        let (usdc, usdc_client) = create_token_contract(&e, &admin);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &admin, &blnt, &usdc);
        let (backstop_address, backstop_client) =
            create_backstop(&e, &pool, &lp_token, &usdc, &blnt);
        blnt_client.mint(&depositor, &500_001_0000000);
        blnt_client.approve(&depositor, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&depositor, &12_501_0000000);
        usdc_client.approve(&depositor, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &1_500_0000000,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &depositor,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &depositor,
            &pool,
            &1_500_0000000,
        );

        let (asset, _) = create_token_contract(&e, &admin);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_supply = 100_0000000;
        reserve_data.d_supply = 50_0000000;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &asset, &reserve_config, &reserve_data);

        let positions = Positions {
            liabilities: map![&e, (0, 50_0000000)],
            collateral: map![&e, (0, 100_0000000)],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            reserve_data.b_rate = 0;
            storage::set_res_data(&e, &asset, &reserve_data);
            storage::set_user_positions(&e, &supplier, &positions);
            storage::set_user_positions(&e, &backstop_address, &Positions::env_default(&e));

            let mut pool_state = Pool::load(&e);
            let mut user = User::load(&e, &supplier);
            assert!(check_and_handle_user_bad_debt(
                &e,
                &mut pool_state,
                &supplier,
                &mut user,
            ));
            user.store(&e);
            pool_state.store_cached_reserves(&e);

            assert!(user.positions.collateral.is_empty());
            assert!(user.positions.liabilities.is_empty());
            assert_eq!(storage::get_res_data(&e, &asset).b_supply, 0);
            assert_eq!(
                storage::get_user_positions(&e, &backstop_address)
                    .liabilities
                    .get(0),
                Some(50_0000000),
            );
        });
    }

    #[test]
    fn test_check_and_handle_user_bad_debt() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths();

        let pool = create_pool(&e);
        let bombadil = Address::generate(&e);
        let frodo = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnt, blnt_client) = create_blnt_token(&e, &pool, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnt, &usdc);
        let (backstop_address, backstop_client) =
            create_backstop(&e, &pool, &lp_token, &usdc, &blnt);

        // mint lp tokens and deposit them into the pool's backstop
        let backstop_tokens = 1_500_0000000; // over 5% of threshold
        blnt_client.mint(&frodo, &500_001_0000000);
        blnt_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&frodo, &12_501_0000000);
        usdc_client.approve(&frodo, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &backstop_tokens,
            &vec![&e, 500_001_0000000, 12_501_0000000],
            &frodo,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::SecondLoss,
            &frodo,
            &pool,
            &backstop_tokens,
        );

        let (underlying_0, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_0) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_0, &reserve_config, &reserve_data_0);

        let (underlying_1, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, reserve_data_1) = testutils::default_reserve_meta();
        testutils::create_reserve(&e, &pool, &underlying_1, &reserve_config, &reserve_data_1);

        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });
        let pool_config = PoolConfig {
            oracle: Address::generate(&e),
            min_collateral: 1_0000000,
            bstop_rate: 0_1000000,
            status: 1,
            max_positions: 5,
        };
        let positions = Positions {
            liabilities: map![&e, (0, 1_5000000), (1, 50_987_654_321)],
            collateral: map![&e],
            supply: map![&e, (0, 100_1234567)],
        };
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 0_5000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(&e, &pool_config);
            storage::set_user_positions(&e, &samwise, &positions);
            storage::set_user_positions(&e, &backstop_address, &backstop_positions);

            let mut pool = Pool::load(&e);
            let mut user = User::load(&e, &samwise);

            let result = check_and_handle_user_bad_debt(&e, &mut pool, &samwise, &mut user);
            assert_eq!(result, true);

            // assert user forgiven liabilities and assigned to backstop
            assert_eq!(user.positions.liabilities.len(), 0);
            assert_eq!(user.positions.collateral.len(), 0);
            assert_eq!(user.positions.supply, positions.supply);

            let post_backstop_positions = storage::get_user_positions(&e, &backstop_address);
            assert_eq!(
                post_backstop_positions.liabilities,
                map![&e, (0, 0_5000000 + 1_5000000), (1, 50_987_654_321)]
            );
            assert_eq!(post_backstop_positions.collateral.len(), 0);
            assert_eq!(post_backstop_positions.supply.len(), 0);

            // store pool reserves and assert they got updated
            pool.store_cached_reserves(&e);
            let post_reserve_data_0 = storage::get_res_data(&e, &underlying_0);
            assert_eq!(post_reserve_data_0.last_time, 100);
            assert_eq!(post_reserve_data_0.d_supply, reserve_data_0.d_supply);
            assert!(post_reserve_data_0.d_rate > reserve_data_0.d_rate);
            assert_eq!(post_reserve_data_0.b_supply, reserve_data_0.b_supply);
            assert!(post_reserve_data_0.b_rate > reserve_data_0.b_rate);
            let post_reserve_data_1 = storage::get_res_data(&e, &underlying_1);
            assert_eq!(post_reserve_data_1.last_time, 100);
            assert_eq!(post_reserve_data_1.d_supply, reserve_data_1.d_supply);
            assert!(post_reserve_data_1.d_rate > reserve_data_1.d_rate);
            assert_eq!(post_reserve_data_0.b_supply, reserve_data_0.b_supply);
            assert!(post_reserve_data_0.b_rate > reserve_data_0.b_rate);
        });
    }
}
