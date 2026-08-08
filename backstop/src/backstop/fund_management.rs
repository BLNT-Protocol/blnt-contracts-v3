use crate::{contract::require_nonnegative, emissions, storage, BackstopError};
use sep_41_token::TokenClient;
use soroban_sdk::{panic_with_error, Address, Env};

use super::{require_is_from_pool_factory, tier_token, BackstopTier};

/// Perform a draw from a pool's backstop
///
/// `pool_address` MUST be authenticated before calling
pub fn execute_draw(
    e: &Env,
    tier: BackstopTier,
    pool_address: &Address,
    amount: i128,
    to: &Address,
) {
    require_nonnegative(e, amount);
    if amount == 0 {
        return;
    }
    emissions::prepare_pool_weight_change(e, tier, pool_address);

    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);

    pool_balance.withdraw(e, amount, 0);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, tier, pool_address);

    TokenClient::new(e, &tier_token(e, tier)).transfer(&e.current_contract_address(), to, &amount);
}

/// Perform a donation to one tier of a pool's backstop.
pub fn execute_donate(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_address: &Address,
    amount: i128,
) {
    require_nonnegative(e, amount);
    if from == pool_address || from == &e.current_contract_address() {
        panic_with_error!(e, &BackstopError::BadRequest)
    }
    emissions::prepare_pool_weight_change(e, tier, pool_address);

    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);
    require_is_from_pool_factory(e, pool_address, pool_balance.shares);

    TokenClient::new(e, &tier_token(e, tier)).transfer_from(
        &e.current_contract_address(),
        from,
        &e.current_contract_address(),
        &amount,
    );

    pool_balance.deposit(amount, 0);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, tier, pool_address);
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address};

    use crate::{
        backstop::{execute_deposit_for_tier, BackstopTier},
        testutils::{
            create_backstop, create_backstop_token, create_blnd_xlm_token, create_mock_pool_factory,
        },
    };

    use super::*;

    #[test]
    fn test_execute_donate() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_blnd_xlm_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndXlm, &frodo, &pool_0_id, 25_0000000);
        });

        backstop_token_client.approve(&samwise, &backstop_id, &30_0000000, &e.ledger().sequence());
        e.as_contract(&backstop_id, || {
            execute_donate(&e, BackstopTier::BlndXlm, &samwise, &pool_0_id, 30_0000000);

            let new_pool_balance =
                storage::get_pool_balance_for_tier(&e, BackstopTier::BlndXlm, &pool_0_id);
            assert_eq!(new_pool_balance.shares, 25_0000000);
            assert_eq!(new_pool_balance.tokens, 55_0000000);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_execute_donate_negative_amount() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_0_id, 25_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &samwise,
                &pool_0_id,
                -30_0000000,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1000)")]
    fn test_execute_donate_from_is_to() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_0_id, 25_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &pool_0_id,
                &pool_0_id,
                10_0000000,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1000)")]
    fn test_execute_donate_from_is_self() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_0_id, 25_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::BlndUsdc,
                &backstop_id,
                &pool_0_id,
                10_0000000,
            );
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1004)")]
    fn test_execute_donate_not_pool() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);
        backstop_token_client.mint(&frodo, &100_0000000);

        create_mock_pool_factory(&e, &backstop_id);

        e.as_contract(&backstop_id, || {
            execute_donate(&e, BackstopTier::BlndUsdc, &samwise, &pool_0_id, 30_0000000);
        });
    }

    #[test]
    fn test_execute_draw() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_address = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_address, &bombadil);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_address);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_address, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_0_id, 50_0000000);
        });

        e.as_contract(&backstop_address, || {
            execute_draw(&e, BackstopTier::BlndUsdc, &pool_0_id, 30_0000000, &samwise);

            let new_pool_balance = storage::get_pool_balance(&e, &pool_0_id);
            assert_eq!(new_pool_balance.shares, 50_0000000);
            assert_eq!(new_pool_balance.tokens, 20_0000000);
            assert_eq!(backstop_token_client.balance(&backstop_address), 20_0000000);
            assert_eq!(backstop_token_client.balance(&samwise), 30_0000000);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1003)")]
    fn test_execute_draw_only_can_take_from_pool() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let pool_1_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);
        mock_pool_factory_client.set_mock_pool(&pool_1_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_0_id, 50_0000000);
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_1_id, 50_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_draw(&e, BackstopTier::BlndUsdc, &pool_0_id, 51_0000000, &samwise);
        });
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_execute_draw_negative_amount() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool_0_id = Address::generate(&e);
        let bombadil = Address::generate(&e);
        let samwise = Address::generate(&e);
        let frodo = Address::generate(&e);

        let (_, backstop_token_client) = create_backstop_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_mock_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit_for_tier(&e, BackstopTier::BlndUsdc, &frodo, &pool_0_id, 50_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_draw(
                &e,
                BackstopTier::BlndUsdc,
                &pool_0_id,
                -30_0000000,
                &samwise,
            );
        });
    }
}
