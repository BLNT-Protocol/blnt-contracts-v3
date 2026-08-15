use crate::{
    constants::MAX_Q4W_SIZE, contract::require_nonnegative, dependencies::PoolClient, emissions,
    storage, BackstopError,
};
use sep_41_token::TokenClient;
use soroban_sdk::{panic_with_error, unwrap::UnwrapOptimized, Address, Env};

use super::{tier_token, BackstopTier, Q4W};

/// Perform a queue for withdrawal from one fixed backstop tier.
pub fn execute_queue_withdrawal(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_address: &Address,
    amount: i128,
) -> Q4W {
    require_nonnegative(e, amount);
    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);
    let mut user_balance = storage::get_user_balance_for_tier(e, tier, pool_address, from);
    require_q4w_entry_capacity(e, from, pool_address);
    user_balance.queue_shares_for_withdrawal(e, amount);

    emissions::prepare_pool_weight_change(e, tier);
    emissions::checkpoint_user_ongoing_for_weight_change(e, tier, from, pool_address);
    pool_balance.queue_for_withdraw(amount);

    storage::set_user_balance_for_tier(e, tier, pool_address, from, &user_balance);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, tier, pool_address);

    user_balance.q4w.last().unwrap_optimized()
}

/// Dequeue a withdrawal from one fixed backstop tier.
pub fn execute_dequeue_withdrawal(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_address: &Address,
    amount: i128,
) {
    require_nonnegative(e, amount);
    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);
    let mut user_balance = storage::get_user_balance_for_tier(e, tier, pool_address, from);
    user_balance.dequeue_shares(e, amount);

    emissions::prepare_pool_weight_change(e, tier);
    emissions::checkpoint_user_ongoing_for_weight_change(e, tier, from, pool_address);
    user_balance.add_shares(amount);
    pool_balance.dequeue_q4w(e, amount);

    storage::set_user_balance_for_tier(e, tier, pool_address, from, &user_balance);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, tier, pool_address);
}

/// Withdraw expired shares from one fixed backstop tier.
pub fn execute_withdraw(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_address: &Address,
    amount: i128,
    to: &Address,
) -> i128 {
    require_nonnegative(e, amount);
    let pool_client = PoolClient::new(e, pool_address);
    let backstop_positions = pool_client.get_positions(&e.current_contract_address());
    if !backstop_positions.liabilities.is_empty() {
        panic_with_error!(e, &BackstopError::BadDebtExists);
    }

    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);
    let mut user_balance = storage::get_user_balance_for_tier(e, tier, pool_address, from);

    user_balance.withdraw_shares(e, amount);

    let to_return = pool_balance.convert_to_tokens(amount);
    if to_return == 0 && pool_balance.tokens != 0 {
        panic_with_error!(e, &BackstopError::InvalidTokenWithdrawAmount);
    }

    emissions::prepare_pool_weight_change(e, tier);
    emissions::checkpoint_user_ongoing_for_weight_change(e, tier, from, pool_address);
    pool_balance.withdraw(e, to_return, amount);
    storage::set_user_balance_for_tier(e, tier, pool_address, from, &user_balance);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, tier, pool_address);

    if to_return > 0 {
        let backstop_token_client = TokenClient::new(e, &tier_token(e, tier));
        backstop_token_client.transfer(&e.current_contract_address(), to, &to_return);
    }

    to_return
}

fn require_q4w_entry_capacity(e: &Env, from: &Address, pool: &Address) {
    let mut entries = 0;
    for tier in [
        BackstopTier::BlndUsdc,
        BackstopTier::BlndXlm,
        BackstopTier::Usdc,
    ] {
        entries += storage::get_user_balance_for_tier(e, tier, pool, from)
            .q4w
            .len();
    }
    if entries >= MAX_Q4W_SIZE {
        panic_with_error!(e, BackstopError::TooManyQ4WEntries);
    }
}

#[cfg(test)]
mod tests {
    use mock_pool::Positions;
    use soroban_sdk::{
        map,
        testutils::{Address as _, Ledger, LedgerInfo},
        vec, Address,
    };

    use crate::{
        backstop::{execute_deposit, execute_donate, execute_draw},
        testutils::{
            assert_eq_vec_q4w, create_backstop, create_backstop_token, create_mock_pool,
            create_mock_pool_factory,
        },
    };

    use super::*;

    #[test]
    fn test_execute_queue_withdrawal() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        // setup pool with deposits
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                100_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                42_0000000,
            );

            let new_user_balance = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(new_user_balance.shares, 58_0000000);
            let expected_q4w = vec![
                &e,
                Q4W {
                    amount: 42_0000000,
                    exp: 10000 + 17 * 24 * 60 * 60,
                },
            ];
            assert_eq_vec_q4w(&new_user_balance.q4w, &expected_q4w);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(new_pool_balance.q4w, 42_0000000);
            assert_eq!(new_pool_balance.shares, 100_0000000);
            assert_eq!(new_pool_balance.tokens, 100_0000000);

            assert_eq!(
                backstop_token_client.balance(&backstop_address),
                100_0000000
            );
            assert_eq!(backstop_token_client.balance(&samwise), 0);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_execute_queue_withdrawal_negative_amount() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        // setup pool with deposits
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                100_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                -42_0000000,
            );
        });
    }

    #[test]
    fn test_execute_dequeue_withdrawal() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        // queue shares for withdraw
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                75_0000000,
            );

            e.ledger().set(LedgerInfo {
                protocol_version: 27,
                sequence_number: 100,
                timestamp: 10000,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 10,
                min_persistent_entry_ttl: 10,
                max_entry_ttl: 3110400,
            });

            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                25_0000000,
            );

            e.ledger().set(LedgerInfo {
                protocol_version: 27,
                sequence_number: 100,
                timestamp: 20000,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 10,
                min_persistent_entry_ttl: 10,
                max_entry_ttl: 3110400,
            });

            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                40_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 30000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            execute_dequeue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                30_0000000,
            );

            let new_user_balance = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(new_user_balance.shares, 40_0000000);
            let expected_q4w = vec![
                &e,
                Q4W {
                    amount: 25_0000000,
                    exp: 10000 + 17 * 24 * 60 * 60,
                },
                Q4W {
                    amount: 10_0000000,
                    exp: 20000 + 17 * 24 * 60 * 60,
                },
            ];
            assert_eq_vec_q4w(&new_user_balance.q4w, &expected_q4w);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(new_pool_balance.q4w, 35_0000000);
            assert_eq!(new_pool_balance.shares, 75_0000000);
            assert_eq!(new_pool_balance.tokens, 75_0000000);
        });
    }
    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_execute_dequeue_withdrawal_negative_amount() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let pool_address = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        // queue shares for withdraw
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                75_0000000,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                25_0000000,
            );

            e.ledger().set(LedgerInfo {
                protocol_version: 27,
                sequence_number: 100,
                timestamp: 10000,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 10,
                min_persistent_entry_ttl: 10,
                max_entry_ttl: 3110400,
            });

            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                40_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 20000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            execute_dequeue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                -30_0000000,
            );
        });
    }

    #[test]
    fn test_execute_withdrawal() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let (pool_address, _) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &150_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        backstop_token_client.approve(
            &samwise,
            &backstop_address,
            &50_0000000,
            &e.ledger().sequence(),
        );
        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                100_0000000,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                42_0000000,
            );
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                50_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            let tokens = execute_withdraw(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                42_0000000,
                &samwise,
            );

            let new_user_balance = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(new_user_balance.shares, 100_0000000 - 42_0000000);
            assert_eq!(new_user_balance.q4w.len(), 0);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(new_pool_balance.q4w, 0);
            assert_eq!(new_pool_balance.shares, 100_0000000 - 42_0000000);
            assert_eq!(new_pool_balance.tokens, 150_0000000 - tokens);
            assert_eq!(tokens, 63_0000000);

            assert_eq!(
                backstop_token_client.balance(&backstop_address),
                150_0000000 - tokens
            );
            assert_eq!(backstop_token_client.balance(&samwise), tokens);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_execute_withdrawal_negative_amount() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let (pool_address, _) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &150_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        backstop_token_client.approve(
            &samwise,
            &backstop_address,
            &50_0000000,
            &e.ledger().sequence(),
        );
        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                100_0000000,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                42_0000000,
            );
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                50_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            execute_withdraw(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                -42_0000000,
                &samwise,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1006)")]
    fn test_execute_withdrawal_zero_tokens() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let (pool_address, _) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &150_0000000);
        backstop_token_client.mint(&frodo, &150_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        e.as_contract(&backstop_address, || {
            execute_deposit(&e, BackstopTier::BlndUsdc, &frodo, &pool_address, 1_0000001);
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                1_0000000,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                1_0000000,
            );
            execute_draw(&e, BackstopTier::BlndUsdc, &pool_address, 1_9999999, &frodo);
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            execute_withdraw(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                1_0000000,
                &samwise,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1011)")]
    fn test_execute_withdrawal_bad_debt_exists() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let (pool_address, mock_pool_client) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &150_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        // give the backstop bad debt
        let backstop_positions = Positions {
            liabilities: map![&e, (0, 1_0000000)],
            collateral: map![&e],
            supply: map![&e],
        };
        mock_pool_client.set_positions(&backstop_address, &backstop_positions);

        backstop_token_client.approve(
            &samwise,
            &backstop_address,
            &50_0000000,
            &e.ledger().sequence(),
        );

        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                100_0000000,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                42_0000000,
            );
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                50_0000000,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            let tokens = execute_withdraw(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                42_0000000,
                &samwise,
            );

            let new_user_balance = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(new_user_balance.shares, 100_0000000 - 42_0000000);
            assert_eq!(new_user_balance.q4w.len(), 0);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(new_pool_balance.q4w, 0);
            assert_eq!(new_pool_balance.shares, 100_0000000 - 42_0000000);
            assert_eq!(new_pool_balance.tokens, 150_0000000 - tokens);
            assert_eq!(tokens, 63_0000000);

            assert_eq!(
                backstop_token_client.balance(&backstop_address),
                150_0000000 - tokens
            );
            assert_eq!(backstop_token_client.balance(&samwise), tokens);
        });
    }

    #[test]
    fn test_execute_withdrawal_burns_expired_shares_in_drained_backstop() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop_address = create_backstop(&e);
        let (pool_address, _) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&samwise, &150_0000000);
        backstop_token_client.mint(&frodo, &150_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        e.as_contract(&backstop_address, || {
            execute_deposit(&e, BackstopTier::BlndUsdc, &frodo, &pool_address, 1_0000001);
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                1_0000000,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                1_0000000,
            );
            execute_draw(&e, BackstopTier::BlndUsdc, &pool_address, 2_0000001, &frodo);
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            assert_eq!(
                execute_withdraw(
                    &e,
                    BackstopTier::BlndUsdc,
                    &samwise,
                    &pool_address,
                    1_0000000,
                    &samwise,
                ),
                0
            );
            let user = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(user.shares, 0);
            assert_eq!(user.q4w.len(), 0);
            let pool = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(pool.tokens, 0);
            assert_eq!(pool.shares, 1_0000001);
            assert_eq!(pool.q4w, 0);
        });
    }

    #[test]
    fn test_execute_withdrawal_all_shares_over_1_rate() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_address = create_backstop(&e);
        let (pool_address, _) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        let deposit_amount = 111_1111111;
        let donate_amount = 123;
        backstop_token_client.mint(&samwise, &(deposit_amount + donate_amount));
        backstop_token_client.approve(
            &samwise,
            &backstop_address,
            &donate_amount,
            &e.ledger().sequence(),
        );
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                deposit_amount,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                deposit_amount,
            );
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                donate_amount,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 201,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            let tokens = execute_withdraw(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                deposit_amount,
                &samwise,
            );

            let new_user_balance = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(new_user_balance.shares, 0);
            assert_eq!(new_user_balance.q4w.len(), 0);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(new_pool_balance.q4w, 0);
            assert_eq!(new_pool_balance.shares, 0);
            assert_eq!(new_pool_balance.tokens, 0);
            assert_eq!(tokens, deposit_amount + donate_amount);

            assert_eq!(backstop_token_client.balance(&backstop_address), 0);
            assert_eq!(backstop_token_client.balance(&samwise), tokens);
        });
    }

    #[test]
    fn test_execute_withdrawal_all_shares_under_1_rate() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 200,
            timestamp: 10000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        let backstop_address = create_backstop(&e);
        let (pool_address, _) = create_mock_pool(&e, &backstop_address);

        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_pool(&pool_address);

        // setup pool with queue for withdrawal and allow the backstop to incur a profit
        let deposit_amount = 111_1111111;
        let draw_amount = 123;
        backstop_token_client.mint(&samwise, &deposit_amount);
        e.as_contract(&backstop_address, || {
            execute_deposit(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                deposit_amount,
            );
            execute_queue_withdrawal(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                deposit_amount,
            );
            execute_draw(
                &e,
                BackstopTier::BlndUsdc,
                &pool_address,
                draw_amount,
                &samwise,
            );
        });

        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 201,
            timestamp: 10000 + 17 * 24 * 60 * 60 + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3110400,
        });

        e.as_contract(&backstop_address, || {
            let tokens = execute_withdraw(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_address,
                deposit_amount,
                &samwise,
            );

            let new_user_balance = storage::get_user_balance(&e, &pool_address, &samwise);
            assert_eq!(new_user_balance.shares, 0);
            assert_eq!(new_user_balance.q4w.len(), 0);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_address);
            assert_eq!(new_pool_balance.q4w, 0);
            assert_eq!(new_pool_balance.shares, 0);
            assert_eq!(new_pool_balance.tokens, 0);
            assert_eq!(tokens, deposit_amount - draw_amount);

            assert_eq!(backstop_token_client.balance(&backstop_address), 0);
            assert_eq!(backstop_token_client.balance(&samwise), deposit_amount);
        });
    }
}
