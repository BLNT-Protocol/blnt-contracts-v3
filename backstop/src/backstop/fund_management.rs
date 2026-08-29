use crate::{
    constants::{BUYBACK_HAIRCUT_DENOMINATOR, BUYBACK_HAIRCUT_NUMERATOR},
    contract::require_nonnegative,
    emissions, storage, BackstopError,
};
use sep_41_token::TokenClient;
use soroban_sdk::{panic_with_error, Address, Env};

use super::{require_is_from_pool_factory, tier_asset, tier_token, BackstopAsset, BackstopTier};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DonationResult {
    pub credited: i128,
    pub buyback: i128,
    pub pending_buyback: i128,
}

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
    let emission_eligible = emissions::prepare_pool_weight_change(e, tier, pool_address);

    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);

    pool_balance.withdraw(e, amount, 0);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, pool_address, emission_eligible);

    TokenClient::new(e, &tier_token(e, pool_address, tier)).transfer(
        &e.current_contract_address(),
        to,
        &amount,
    );
}

/// Perform a donation to one tier of a pool's backstop.
pub fn execute_donate(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_address: &Address,
    amount: i128,
) -> DonationResult {
    require_nonnegative(e, amount);
    if from == pool_address || from == &e.current_contract_address() {
        panic_with_error!(e, &BackstopError::BadRequest)
    }
    let emission_eligible = emissions::prepare_pool_weight_change(e, tier, pool_address);

    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool_address);
    require_is_from_pool_factory(e, pool_address, pool_balance.shares);

    TokenClient::new(e, &tier_token(e, pool_address, tier)).transfer_from(
        &e.current_contract_address(),
        from,
        &e.current_contract_address(),
        &amount,
    );

    let asset = tier_asset(e, pool_address, tier);
    let (credited, buyback, pending_buyback) =
        if matches!(asset, BackstopAsset::Usdc | BackstopAsset::Xlm) {
            let carry = storage::get_buyback_carry(e, pool_address, tier);
            let haircut_numerator = amount
                .checked_mul(BUYBACK_HAIRCUT_NUMERATOR)
                .and_then(|value| value.checked_add(carry))
                .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
            let buyback = haircut_numerator / BUYBACK_HAIRCUT_DENOMINATOR;
            let next_carry = haircut_numerator % BUYBACK_HAIRCUT_DENOMINATOR;
            let credited = amount
                .checked_sub(buyback)
                .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
            let pending = storage::get_buyback_pending(e, asset)
                .checked_add(buyback)
                .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
            storage::set_buyback_carry(e, pool_address, tier, next_carry);
            storage::set_buyback_pending(e, asset, pending);
            (credited, buyback, pending)
        } else {
            (amount, 0, 0)
        };

    pool_balance.deposit(credited, 0);
    storage::set_pool_balance_for_tier(e, tier, pool_address, &pool_balance);
    emissions::finish_pool_weight_change(e, pool_address, emission_eligible);

    DonationResult {
        credited,
        buyback,
        pending_buyback,
    }
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address};

    use crate::{
        backstop::{execute_deposit, BackstopTier},
        testutils::{
            create_backstop, create_backstop_token, create_blnt_xlm_token, create_mock_pool_factory,
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

        let (_, backstop_token_client) = create_blnt_xlm_token(&e, &backstop_id, &bombadil);
        backstop_token_client.mint(&samwise, &100_0000000);
        backstop_token_client.mint(&frodo, &100_0000000);

        let (_, mock_pool_factory_client) = create_mock_pool_factory(&e, &backstop_id);
        mock_pool_factory_client.set_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::FirstLoss, &frodo, &pool_0_id, 25_0000000);
        });

        backstop_token_client.approve(&samwise, &backstop_id, &30_0000000, &e.ledger().sequence());
        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::FirstLoss,
                &samwise,
                &pool_0_id,
                30_0000000,
            );

            let new_pool_balance =
                storage::get_pool_balance_for_tier(&e, BackstopTier::FirstLoss, &pool_0_id);
            assert_eq!(new_pool_balance.shares, 25_0000000);
            assert_eq!(new_pool_balance.tokens, 55_0000000);
            assert_eq!(storage::get_buyback_pending(&e, BackstopAsset::Usdc), 0);
            assert_eq!(storage::get_buyback_pending(&e, BackstopAsset::Xlm), 0);
        });
    }

    #[test]
    fn test_execute_usdc_donate_retains_one_percent_buyback_with_carry() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.cost_estimate().budget().reset_unlimited();

        let backstop_id = create_backstop(&e);
        let pool = Address::generate(&e);
        let admin = Address::generate(&e);
        let depositor = Address::generate(&e);
        let donor = Address::generate(&e);
        let (_, usdc) = crate::testutils::create_usdc_token(&e, &backstop_id, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop_id);
        factory.set_pool(&pool);
        usdc.mint(&depositor, &1_000);
        usdc.mint(&donor, &200);
        usdc.approve(
            &donor,
            &backstop_id,
            &200,
            &e.ledger().sequence().saturating_add(1_000),
        );

        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::ThirdLoss, &depositor, &pool, 1_000);

            let first = execute_donate(&e, BackstopTier::ThirdLoss, &donor, &pool, 150);
            assert_eq!(first.credited, 149);
            assert_eq!(first.buyback, 1);
            assert_eq!(first.pending_buyback, 1);
            assert_eq!(
                storage::get_buyback_carry(&e, &pool, BackstopTier::ThirdLoss),
                50
            );

            let second = execute_donate(&e, BackstopTier::ThirdLoss, &donor, &pool, 50);
            assert_eq!(second.credited, 49);
            assert_eq!(second.buyback, 1);
            assert_eq!(second.pending_buyback, 2);
            assert_eq!(
                storage::get_buyback_carry(&e, &pool, BackstopTier::ThirdLoss),
                0
            );
            assert_eq!(storage::get_buyback_pending(&e, BackstopAsset::Usdc), 2);

            let balance = storage::get_pool_balance_for_tier(&e, BackstopTier::ThirdLoss, &pool);
            assert_eq!(balance.shares, 1_000);
            assert_eq!(balance.tokens, 1_198);
        });
        assert_eq!(usdc.balance(&backstop_id), 1_200);
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
        mock_pool_factory_client.set_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_0_id, 25_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::SecondLoss,
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
        mock_pool_factory_client.set_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_0_id, 25_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::SecondLoss,
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
        mock_pool_factory_client.set_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_0_id, 25_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_donate(
                &e,
                BackstopTier::SecondLoss,
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
            execute_donate(
                &e,
                BackstopTier::SecondLoss,
                &samwise,
                &pool_0_id,
                30_0000000,
            );
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
        mock_pool_factory_client.set_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_address, || {
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_0_id, 50_0000000);
        });

        e.as_contract(&backstop_address, || {
            execute_draw(
                &e,
                BackstopTier::SecondLoss,
                &pool_0_id,
                30_0000000,
                &samwise,
            );

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
        mock_pool_factory_client.set_pool(&pool_0_id);
        mock_pool_factory_client.set_pool(&pool_1_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_0_id, 50_0000000);
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_1_id, 50_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_draw(
                &e,
                BackstopTier::SecondLoss,
                &pool_0_id,
                51_0000000,
                &samwise,
            );
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
        mock_pool_factory_client.set_pool(&pool_0_id);

        // initialize pool 0 with funds
        e.as_contract(&backstop_id, || {
            execute_deposit(&e, BackstopTier::SecondLoss, &frodo, &pool_0_id, 50_0000000);
        });

        e.as_contract(&backstop_id, || {
            execute_draw(
                &e,
                BackstopTier::SecondLoss,
                &pool_0_id,
                -30_0000000,
                &samwise,
            );
        });
    }
}
