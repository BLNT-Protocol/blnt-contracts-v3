use sep_41_token::TokenClient;
use soroban_fixed_point_math::SorobanFixedPoint;
use soroban_sdk::{panic_with_error, Address, Env};

use crate::{constants::SCALAR_12, PoolError};

use super::{Pool, RequestType, Reserve};

/// Return the pool's actual token balance less the reserve's accrued expected
/// cash. A negative value is an unreconciled custody loss.
pub(crate) fn reserve_balance_delta(e: &Env, reserve: &Reserve) -> i128 {
    let pool_token_balance =
        TokenClient::new(e, &reserve.asset).balance(&e.current_contract_address());
    let reserve_token_balance = reserve
        .total_supply(e)
        .checked_add(reserve.data.backstop_credit)
        .and_then(|value| value.checked_sub(reserve.total_liabilities(e)))
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    pool_token_balance
        .checked_sub(reserve_token_balance)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
}

/// Require that a reserve has no unrecognized custody deficit.
pub(crate) fn require_reconciled(e: &Env, reserve: &Reserve) {
    if reserve_balance_delta(e, reserve) < 0 {
        panic_with_error!(e, PoolError::UnreconciledReserveLoss);
    }
}

/// Gulps the excess tokens in the pool, determined by the difference between the pool token balance
/// and the reserve total supply, backstop credit, and liabiltiies.
///
/// ### Arguments
/// * `asset` - The address of the asset to gulp
///
/// ### Returns
/// * The gulped token delta accrued to the backstop credit
///
/// ### Panics
/// * If borrowing is not enabled on the pool. This ensures that the backstop can safely process
/// interest auctions.
pub fn execute_gulp(e: &Env, asset: &Address) -> i128 {
    let pool = Pool::load(e);

    // ensure the backstop can safely accept new interest
    pool.require_action_allowed(e, RequestType::Borrow as u32);

    let mut reserve = Reserve::load(e, &pool.config, asset);
    let token_balance_delta = reserve_balance_delta(e, &reserve);
    if token_balance_delta <= 0 {
        return 0;
    }

    reserve.data.backstop_credit = reserve
        .data
        .backstop_credit
        .checked_add(token_balance_delta)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    reserve.store(e);

    return token_balance_delta;
}

/// Recognize an unexplained reserve-custody deficit as a proportional loss to
/// the affected reserve's suppliers and then its unpaid take-rate credit.
///
/// Returns `(underlying_loss, supplier_loss, backstop_credit_loss,
/// b_rate_loss, interest_auction_canceled)`. The operation is permissionless,
/// does not change bToken balances or borrower liabilities, and creates no
/// backstop debt.
pub fn execute_reconcile_loss(e: &Env, asset: &Address) -> (i128, i128, i128, i128, bool) {
    let mut pool = Pool::load(e);
    let mut reserve = pool.load_reserve(e, asset, true);
    let balance_delta = reserve_balance_delta(e, &reserve);
    if balance_delta >= 0 {
        pool.cache_reserve(reserve);
        pool.store_cached_reserves(e);
        return (0, 0, 0, 0, false);
    }

    let loss = balance_delta
        .checked_neg()
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    let supplier_claim = reserve.total_supply(e);
    let supplier_loss = core::cmp::min(loss, supplier_claim);

    let previous_b_rate = reserve.data.b_rate;
    if reserve.data.b_supply > 0 && supplier_loss == supplier_claim {
        // Canonicalize complete economic exhaustion. Fixed-point floor and
        // ceiling rounding can otherwise leave a positive b_rate even though
        // the aggregate supplier claim has reached zero.
        reserve.data.b_rate = 0;
    } else if supplier_loss > 0 {
        if reserve.data.b_supply <= 0 {
            panic_with_error!(e, PoolError::BalanceError);
        }
        let b_rate_loss = supplier_loss.fixed_div_ceil(e, &reserve.data.b_supply, &SCALAR_12);
        reserve.data.b_rate = previous_b_rate
            .checked_sub(b_rate_loss)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError))
            .max(0);
    }
    let applied_b_rate_loss = previous_b_rate
        .checked_sub(reserve.data.b_rate)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));

    let remaining_delta = reserve_balance_delta(e, &reserve);
    let mut backstop_credit_loss = 0_i128;
    let mut interest_auction_canceled = false;
    if remaining_delta < 0 {
        backstop_credit_loss = remaining_delta
            .checked_neg()
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        let previous_credit = reserve.data.backstop_credit;
        if backstop_credit_loss > previous_credit {
            panic_with_error!(e, PoolError::BalanceError);
        }
        let new_credit = previous_credit
            .checked_sub(backstop_credit_loss)
            .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
        interest_auction_canceled =
            crate::auctions::reconcile_interest_credit(e, asset, previous_credit, new_credit);
        reserve.data.backstop_credit = new_credit;
    }

    // Ceiling rounding may leave a positive dust delta, but reconciliation
    // must eliminate the complete deficit without touching other reserves or
    // creating backstop liabilities.
    if reserve_balance_delta(e, &reserve) < 0 {
        panic_with_error!(e, PoolError::BalanceError);
    }

    pool.cache_reserve(reserve);
    pool.store_cached_reserves(e);
    (
        loss,
        supplier_loss,
        backstop_credit_loss,
        applied_b_rate_loss,
        interest_auction_canceled,
    )
}

#[cfg(test)]
mod tests {
    use crate::constants::{SCALAR_12, SCALAR_7};
    use crate::pool::{execute_gulp, Positions, Request, RequestType};
    use crate::storage::{self, PoolConfig};
    use crate::testutils;
    use crate::PoolClient;
    use soroban_sdk::{
        map,
        testutils::{Address as _, Ledger, LedgerInfo},
        vec, Address, Env, Map,
    };

    #[test]
    fn test_execute_gulp() {
        let e = Env::default();
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
        let bombadil = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);

        let initial_backstop_credit = 500;
        let (underlying, underlying_client) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = 1_000_000_000_000;
        reserve_data.d_rate = 1_000_000_000_000;
        reserve_data.d_supply = 500 * SCALAR_7;
        reserve_data.b_supply = 1000 * SCALAR_7;
        reserve_data.backstop_credit = initial_backstop_credit;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        let additional_tokens = 10 * SCALAR_7;
        underlying_client.mint(&pool, &additional_tokens);
        e.as_contract(&pool, || {
            let pool_config = PoolConfig {
                oracle,
                min_collateral: 1_0000000,
                bstop_rate: 0_1000000,
                status: 1,
                max_positions: 4,
            };
            storage::set_pool_config(&e, &pool_config);

            let token_delta_result = execute_gulp(&e, &underlying);
            assert_eq!(token_delta_result, additional_tokens);

            let new_reserve_data = storage::get_res_data(&e, &underlying);
            assert_eq!(new_reserve_data.last_time, 100);
            assert_eq!(
                new_reserve_data.backstop_credit,
                additional_tokens + initial_backstop_credit
            );
        });
    }

    #[test]
    fn test_execute_gulp_credits_surplus_with_zero_supply() {
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });
        let admin = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);
        let (underlying, token) = testutils::create_token_contract(&e, &admin);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = 0;
        reserve_data.b_supply = 0;
        reserve_data.d_supply = 50 * SCALAR_7;
        reserve_data.backstop_credit = 50 * SCALAR_7;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        let surplus = 10 * SCALAR_7;
        token.mint(&pool, &surplus);
        e.as_contract(&pool, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0_2000000,
                    status: 1,
                    max_positions: 4,
                },
            );

            assert_eq!(execute_gulp(&e, &underlying), surplus);
            let after = storage::get_res_data(&e, &underlying);
            assert_eq!(after.b_supply, 0);
            assert_eq!(after.b_rate, 0);
            assert_eq!(after.d_supply, 50 * SCALAR_7);
            assert_eq!(after.backstop_credit, 60 * SCALAR_7);
        });
    }

    #[test]
    fn test_execute_gulp_accrues_interest_before_gulp() {
        let e = Env::default();
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
        let bombadil = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);

        let initial_backstop_credit = 500;
        let (underlying, underlying_client) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = 1_000_000_000_000;
        reserve_data.d_rate = 1_000_000_000_000;
        reserve_data.d_supply = 500 * SCALAR_7;
        reserve_data.b_supply = 1000 * SCALAR_7;
        reserve_data.backstop_credit = initial_backstop_credit;
        reserve_data.last_time = 0;
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        let additional_tokens = 10 * SCALAR_7;
        underlying_client.mint(&pool, &additional_tokens);
        e.as_contract(&pool, || {
            let pool_config = PoolConfig {
                oracle,
                min_collateral: 1_0000000,
                bstop_rate: 0_1000000,
                status: 0,
                max_positions: 4,
            };
            storage::set_pool_config(&e, &pool_config);

            let token_delta_result = execute_gulp(&e, &underlying);
            assert_eq!(token_delta_result, additional_tokens);

            let new_reserve_data = storage::get_res_data(&e, &underlying);
            assert_eq!(new_reserve_data.b_rate, 1_000_000_000_000 + 62000);
            assert_eq!(new_reserve_data.last_time, 100);
            // 68 is the backstop credit due to the interest accrued
            assert_eq!(
                new_reserve_data.backstop_credit,
                additional_tokens + initial_backstop_credit + 68
            );
        });
    }

    #[test]
    fn test_execute_gulp_zero_delta_skips() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
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
        let bombadil = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);

        let (underlying, _) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = 1_000_000_000_000;
        reserve_data.d_rate = 1_000_000_000_000;
        reserve_data.d_supply = 500 * SCALAR_7;
        reserve_data.b_supply = 1000 * SCALAR_7;
        reserve_data.backstop_credit = 0;
        reserve_data.last_time = 0;
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        e.as_contract(&pool, || {
            let pool_config = PoolConfig {
                oracle,
                min_collateral: 1_0000000,
                bstop_rate: 0_1000000,
                status: 0,
                max_positions: 4,
            };
            storage::set_pool_config(&e, &pool_config);

            let token_delta_result = execute_gulp(&e, &underlying);
            assert_eq!(token_delta_result, 0);

            // data not set
            let new_reserve_data = storage::get_res_data(&e, &underlying);
            assert_eq!(new_reserve_data.b_rate, 1_000_000_000_000);
            assert_eq!(new_reserve_data.last_time, 0);
            assert_eq!(new_reserve_data.backstop_credit, 0);
        });
    }

    #[test]
    fn test_execute_gulp_negative_delta_skips() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
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
        let bombadil = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);

        let (underlying, underlying_client) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = 1_000_000_000_000;
        reserve_data.d_rate = 1_000_000_000_000;
        reserve_data.d_supply = 500 * SCALAR_7;
        reserve_data.b_supply = 1000 * SCALAR_7;
        reserve_data.backstop_credit = 0;
        reserve_data.last_time = 0;
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        underlying_client.burn(&pool, &SCALAR_7);
        e.as_contract(&pool, || {
            let pool_config = PoolConfig {
                oracle,
                min_collateral: 1_0000000,
                bstop_rate: 0_1000000,
                status: 0,
                max_positions: 4,
            };
            storage::set_pool_config(&e, &pool_config);

            let token_delta_result = execute_gulp(&e, &underlying);
            assert_eq!(token_delta_result, 0);

            // data not set
            let new_reserve_data = storage::get_res_data(&e, &underlying);
            assert_eq!(new_reserve_data.b_rate, 1_000_000_000_000);
            assert_eq!(new_reserve_data.last_time, 0);
            assert_eq!(new_reserve_data.backstop_credit, 0);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1206)")]
    fn test_execute_gulp_checks_status() {
        let e = Env::default();
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
        let bombadil = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);

        let initial_backstop_credit = 500;
        let (underlying, underlying_client) = testutils::create_token_contract(&e, &bombadil);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = 1_000_000_000_000;
        reserve_data.d_rate = 1_000_000_000_000;
        reserve_data.d_supply = 500 * SCALAR_7;
        reserve_data.b_supply = 1000 * SCALAR_7;
        reserve_data.backstop_credit = initial_backstop_credit;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &underlying, &reserve_config, &reserve_data);

        let additional_tokens = 10 * SCALAR_7;
        underlying_client.mint(&pool, &additional_tokens);
        e.as_contract(&pool, || {
            let pool_config = PoolConfig {
                oracle,
                min_collateral: 1_0000000,
                bstop_rate: 0_1000000,
                status: 2,
                max_positions: 4,
            };
            storage::set_pool_config(&e, &pool_config);

            execute_gulp(&e, &underlying);
        });
    }

    #[test]
    fn reconcile_loss_haircuts_all_suppliers_proportionally() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });
        let admin = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);
        let (asset, token) = testutils::create_token_contract(&e, &admin);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_supply = 200 * SCALAR_7;
        reserve_data.d_supply = 50 * SCALAR_7;
        reserve_data.backstop_credit = 0;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &asset, &reserve_config, &reserve_data);

        let supplier_a = Address::generate(&e);
        let supplier_b = Address::generate(&e);
        let positions_a = Positions {
            liabilities: Map::new(&e),
            collateral: map![&e, (0u32, 50 * SCALAR_7)],
            supply: map![&e, (0u32, 50 * SCALAR_7)],
        };
        let positions_b = Positions {
            liabilities: Map::new(&e),
            collateral: Map::new(&e),
            supply: map![&e, (0u32, 100 * SCALAR_7)],
        };
        e.as_contract(&pool, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0_2000000,
                    status: 0,
                    max_positions: 4,
                },
            );
            storage::set_user_positions(&e, &supplier_a, &positions_a);
            storage::set_user_positions(&e, &supplier_b, &positions_b);
        });

        token.burn(&pool, &(30 * SCALAR_7));
        let client = PoolClient::new(&e, &pool);
        assert!(client
            .try_submit(
                &supplier_b,
                &supplier_b,
                &supplier_b,
                &vec![
                    &e,
                    Request {
                        request_type: RequestType::Withdraw as u32,
                        address: asset.clone(),
                        amount: SCALAR_7,
                    },
                ],
            )
            .is_err());
        assert!(client
            .try_submit(
                &supplier_b,
                &supplier_b,
                &supplier_b,
                &vec![
                    &e,
                    Request {
                        request_type: RequestType::Supply as u32,
                        address: asset.clone(),
                        amount: SCALAR_7,
                    },
                ],
            )
            .is_err());
        assert_eq!(client.reconcile_loss(&asset), 30 * SCALAR_7);

        let reserve = client.get_reserve(&asset);
        assert_eq!(reserve.data.b_rate, 0_850_000_000_000);
        assert_eq!(reserve.data.b_supply, 200 * SCALAR_7);
        assert_eq!(reserve.data.d_supply, 50 * SCALAR_7);
        assert_eq!(reserve.data.backstop_credit, 0);
        let positions_a_after = client.get_positions(&supplier_a);
        assert_eq!(positions_a_after.liabilities, positions_a.liabilities);
        assert_eq!(positions_a_after.collateral, positions_a.collateral);
        assert_eq!(positions_a_after.supply, positions_a.supply);
        let positions_b_after = client.get_positions(&supplier_b);
        assert_eq!(positions_b_after.liabilities, positions_b.liabilities);
        assert_eq!(positions_b_after.collateral, positions_b.collateral);
        assert_eq!(positions_b_after.supply, positions_b.supply);
        assert_eq!(
            reserve.to_asset_from_b_token(&e, 100 * SCALAR_7),
            85 * SCALAR_7
        );

        // The accounting now matches custody, so the operation is idempotent.
        assert_eq!(client.reconcile_loss(&asset), 0);
        assert_eq!(client.get_reserve(&asset).data.b_rate, 0_850_000_000_000);
    }

    #[test]
    fn reconcile_loss_uses_backstop_credit_after_suppliers_are_exhausted() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });
        let admin = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);
        let (asset, token) = testutils::create_token_contract(&e, &admin);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_supply = 100 * SCALAR_7;
        reserve_data.d_supply = 0;
        reserve_data.backstop_credit = 50 * SCALAR_7;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &asset, &reserve_config, &reserve_data);
        e.as_contract(&pool, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0_2000000,
                    status: 0,
                    max_positions: 4,
                },
            );
        });

        token.burn(&pool, &(120 * SCALAR_7));
        let client = PoolClient::new(&e, &pool);
        assert_eq!(client.reconcile_loss(&asset), 120 * SCALAR_7);
        let reserve = client.get_reserve(&asset);
        assert_eq!(reserve.data.b_rate, 0);
        assert_eq!(reserve.data.backstop_credit, 30 * SCALAR_7);
        assert_eq!(client.reconcile_loss(&asset), 0);
    }

    #[test]
    fn reconcile_loss_canonicalizes_exhausted_supplier_rate_to_zero() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            timestamp: 100,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });
        let admin = Address::generate(&e);
        let pool = testutils::create_pool(&e);
        let (oracle, _) = testutils::create_mock_oracle(&e);
        let (asset, token) = testutils::create_token_contract(&e, &admin);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_rate = SCALAR_12 + 1;
        reserve_data.b_supply = SCALAR_7;
        reserve_data.d_supply = 0;
        reserve_data.backstop_credit = 0;
        reserve_data.last_time = 100;
        testutils::create_reserve(&e, &pool, &asset, &reserve_config, &reserve_data);
        e.as_contract(&pool, || {
            storage::set_pool_config(
                &e,
                &PoolConfig {
                    oracle,
                    min_collateral: SCALAR_7,
                    bstop_rate: 0_2000000,
                    status: 0,
                    max_positions: 4,
                },
            );
        });

        token.burn(&pool, &SCALAR_7);
        let client = PoolClient::new(&e, &pool);
        assert_eq!(client.reconcile_loss(&asset), SCALAR_7);
        let after = client.get_reserve(&asset).data;
        assert_eq!(after.b_supply, SCALAR_7);
        assert_eq!(after.b_rate, 0);
        assert_eq!(after.backstop_credit, 0);
        assert_eq!(client.reconcile_loss(&asset), 0);
    }
}
