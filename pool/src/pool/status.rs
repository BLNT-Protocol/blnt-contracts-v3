use crate::{
    constants::SCALAR_7,
    dependencies::{BackstopClient, BackstopPoolData},
    storage, PoolError,
};
use soroban_sdk::{panic_with_error, Env};

const STATUS_ADMIN_ACTIVE: u32 = 0;
const STATUS_ACTIVE: u32 = 1;
const STATUS_ADMIN_ON_ICE: u32 = 2;
const STATUS_ON_ICE: u32 = 3;
const STATUS_ADMIN_FROZEN: u32 = 4;
const STATUS_FROZEN: u32 = 5;
const STATUS_SETUP: u32 = 6;

const Q4W_ON_ICE_THRESHOLD: i128 = 3_000_000;
const Q4W_ADMIN_ACTIVE_LIMIT: i128 = 5_000_000;
const Q4W_FROZEN_THRESHOLD: i128 = 6_000_000;
const Q4W_ADMIN_ON_ICE_LIMIT: i128 = 7_500_000;
const ACTIVATION_ENTRY_THRESHOLD_USDC: i128 = 12_500 * SCALAR_7;
const ACTIVATION_MAINTENANCE_THRESHOLD_USDC: i128 = 10_000 * SCALAR_7;

/// Update the pool status based on the backstop module
#[allow(clippy::zero_prefixed_literal)]
#[allow(clippy::inconsistent_digit_grouping)]
pub fn execute_update_pool_status(e: &Env) -> u32 {
    let mut pool_config = storage::get_pool_config(e);
    let backstop_id = storage::get_backstop(e);
    let backstop_client = BackstopClient::new(e, &backstop_id);
    let pool_data = backstop_client.pool_data(&e.current_contract_address());
    let met_threshold = meets_activation_threshold(e, pool_config.status, &pool_data);
    let q4w_percentage = pool_data.q4w_pct;

    pool_config.status = match pool_config.status {
        STATUS_SETUP | STATUS_ADMIN_FROZEN => {
            panic_with_error!(e, PoolError::StatusNotAllowed)
        }
        STATUS_ADMIN_ON_ICE => {
            if q4w_percentage >= Q4W_ADMIN_ON_ICE_LIMIT {
                STATUS_FROZEN
            } else {
                STATUS_ADMIN_ON_ICE
            }
        }
        STATUS_ADMIN_ACTIVE => {
            if !met_threshold || q4w_percentage >= Q4W_ADMIN_ACTIVE_LIMIT {
                STATUS_ON_ICE
            } else {
                STATUS_ADMIN_ACTIVE
            }
        }
        STATUS_ACTIVE | STATUS_ON_ICE | STATUS_FROZEN => {
            if q4w_percentage >= Q4W_FROZEN_THRESHOLD {
                STATUS_FROZEN
            } else if !met_threshold || q4w_percentage >= Q4W_ON_ICE_THRESHOLD {
                STATUS_ON_ICE
            } else {
                STATUS_ACTIVE
            }
        }
        _ => panic_with_error!(e, PoolError::InvalidPoolStatus),
    };
    storage::set_pool_config(e, &pool_config);
    pool_config.status
}

/// Admin set the pool status
#[allow(clippy::zero_prefixed_literal)]
#[allow(clippy::inconsistent_digit_grouping)]
pub fn execute_set_pool_status(e: &Env, pool_status: u32) {
    if !matches!(
        pool_status,
        STATUS_ADMIN_ACTIVE | STATUS_ADMIN_ON_ICE | STATUS_ON_ICE | STATUS_ADMIN_FROZEN
    ) {
        panic_with_error!(e, PoolError::BadRequest);
    }
    let mut pool_config = storage::get_pool_config(e);
    let backstop_id = storage::get_backstop(e);
    let backstop_client = BackstopClient::new(e, &backstop_id);
    let pool_data = backstop_client.pool_data(&e.current_contract_address());
    let met_threshold = meets_activation_threshold(e, pool_config.status, &pool_data);
    let transition_allowed = match pool_status {
        STATUS_ADMIN_ACTIVE => met_threshold && pool_data.q4w_pct < Q4W_ADMIN_ACTIVE_LIMIT,
        STATUS_ADMIN_ON_ICE | STATUS_ON_ICE => pool_data.q4w_pct < Q4W_ADMIN_ON_ICE_LIMIT,
        STATUS_ADMIN_FROZEN => true,
        _ => false,
    };
    if !transition_allowed {
        panic_with_error!(e, PoolError::StatusNotAllowed);
    }
    pool_config.status = pool_status;
    storage::set_pool_config(e, &pool_config);
}

fn meets_activation_threshold(e: &Env, current_status: u32, pool_data: &BackstopPoolData) -> bool {
    pool_data.active_value >= required_activation_value(e, current_status)
}

fn required_activation_value(e: &Env, current_status: u32) -> i128 {
    match current_status {
        STATUS_ADMIN_ACTIVE | STATUS_ACTIVE => ACTIVATION_MAINTENANCE_THRESHOLD_USDC,
        STATUS_ADMIN_ON_ICE | STATUS_ON_ICE | STATUS_ADMIN_FROZEN | STATUS_FROZEN
        | STATUS_SETUP => ACTIVATION_ENTRY_THRESHOLD_USDC,
        _ => panic_with_error!(e, PoolError::InvalidPoolStatus),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        storage::PoolConfig,
        testutils::{create_backstop, create_comet_lp_pool, create_pool, create_token_contract},
    };

    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Address};

    #[test]
    fn activation_hysteresis_uses_maintenance_only_while_active() {
        let e = Env::default();
        assert_eq!(
            required_activation_value(&e, STATUS_ADMIN_ACTIVE),
            ACTIVATION_MAINTENANCE_THRESHOLD_USDC
        );
        assert_eq!(
            required_activation_value(&e, STATUS_ACTIVE),
            ACTIVATION_MAINTENANCE_THRESHOLD_USDC
        );
        for status in STATUS_ADMIN_ON_ICE..=STATUS_SETUP {
            assert_eq!(
                required_activation_value(&e, status),
                ACTIVATION_ENTRY_THRESHOLD_USDC
            );
        }
    }

    #[test]
    fn test_set_pool_status_active() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 0);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, 0);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1204)")]
    fn test_set_pool_status_active_blocks_without_backstop_minimum() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

        // mint lp tokens - under limit
        blnd_client.mint(&samwise, &400_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &10_001_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &40_000_0000000,
            &vec![&e, 400_001_0000000, 10_001_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &9_999_9999999,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 0);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1204)")]
    fn test_set_pool_status_active_blocks_with_too_high_q4w() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &30_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 2,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 0);
        });
    }
    #[test]
    fn test_set_pool_status_on_ice() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 2);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, 2);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1204)")]
    fn test_set_pool_status_admin_on_ice_blocks_with_too_high_q4w() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &40_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 5,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 2);
        });
    }
    #[test]
    #[should_panic(expected = "Error(Contract, #1204)")]
    fn test_set_pool_status_backstop_on_ice_blocks_with_too_high_q4w() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &40_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 6,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 3);
        });
    }
    #[test]
    fn test_set_pool_status_frozen() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 4);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, 4);
        });
    }
    #[test]
    #[should_panic(expected = "Error(Contract, #1200)")]
    fn test_set_non_admin_pool_status_panics() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 2,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 1);
        });
    }

    #[test]
    fn test_update_pool_status_active() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 3,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 1);
        });
    }

    #[test]
    fn test_update_pool_status_admin_set_no_changes() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 0);
        });
    }

    #[test]
    fn test_update_pool_status_on_ice_tokens() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

        // mint lp tokens - under limit
        blnd_client.mint(&samwise, &400_001_0000000);
        blnd_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        usdc_client.mint(&samwise, &10_001_0000000);
        usdc_client.approve(&samwise, &lp_token, &i128::MAX, &99999);
        lp_token_client.join_pool(
            &40_000_0000000,
            &vec![&e, 400_001_0000000, 10_001_0000000],
            &samwise,
        );
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &9_999_9999999,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 3);
        });
    }

    #[test]
    fn test_update_pool_status_on_ice_30_q4w() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &15_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 3);
        });
    }

    #[test]
    fn test_update_pool_status_on_ice_30_q4w_admin_active() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &15_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 0);
        });
    }

    #[test]
    fn test_update_pool_status_on_ice_50_q4w_admin_active() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &25_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 0,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 3);
        });
    }

    #[test]
    fn test_update_pool_status_frozen() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &30_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 1,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 5);
        });
    }
    #[test]
    fn test_update_pool_status_frozen_admin_on_ice() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &30_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 2,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 2);
        });
    }

    #[test]
    fn test_update_pool_status_frozen_75_q4w() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &40_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 2,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            let status = execute_update_pool_status(&e);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, status);
            assert_eq!(status, 5);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1204)")]
    fn test_update_pool_status_admin_frozen() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 4,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_update_pool_status(&e);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1204)")]
    fn test_update_pool_status_setup() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 6,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_update_pool_status(&e);
        });
    }

    #[test]
    fn test_admin_update_pool_status_unfreeze() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();

        let pool_id = create_pool(&e);
        let oracle_id = Address::generate(&e);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (blnd, blnd_client) = create_token_contract(&e, &bombadil);
        let (usdc, usdc_client) = create_token_contract(&e, &bombadil);
        let (lp_token, lp_token_client) = create_comet_lp_pool(&e, &bombadil, &blnd, &usdc);
        let (_, backstop_client) = create_backstop(&e, &pool_id, &lp_token, &usdc, &blnd);

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
        backstop_client.deposit(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &50_000_0000000,
        );
        backstop_client.queue_withdrawal(
            &backstop::BackstopTier::BlndUsdc,
            &samwise,
            &pool_id,
            &12_500_0000000,
        );

        let pool_config = PoolConfig {
            oracle: oracle_id,
            min_collateral: 0,
            bstop_rate: 0,
            status: 5,
            max_positions: 4,
        };
        e.as_contract(&pool_id, || {
            storage::set_admin(&e, &bombadil);
            storage::set_pool_config(&e, &pool_config);

            execute_set_pool_status(&e, 0);

            let new_pool_config = storage::get_pool_config(&e);
            assert_eq!(new_pool_config.status, 0);
        });
    }
}
