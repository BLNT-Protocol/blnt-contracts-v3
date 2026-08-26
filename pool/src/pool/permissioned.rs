use sep_41_token::TokenClient;
use soroban_sdk::{panic_with_error, Address, Env};

use crate::{storage, AuctionType, PoolError};

use super::{gulp::require_reconciled, Pool, User};

/// Burn all of an unauthorized user's supply and collateral shares for one
/// reserve and transfer their current underlying value only to that user.
pub fn execute_force_withdrawal(
    e: &Env,
    user: &Address,
    asset: &Address,
) -> (i128, i128, i128, i128) {
    if user == &e.current_contract_address() || user == &storage::get_backstop(e) {
        panic_with_error!(e, PoolError::BadRequest);
    }
    if storage::has_auction(e, &(AuctionType::UserLiquidation as u32), user) {
        panic_with_error!(e, PoolError::AuctionInProgress);
    }

    let mut pool = Pool::load(e);
    let mut user_state = User::load(e, user);
    if user_state.has_liabilities() {
        panic_with_error!(e, PoolError::InvalidLiquidation);
    }

    let mut reserve = pool.load_reserve(e, asset, true);
    require_reconciled(e, &reserve);
    let supply_burned = user_state.get_supply(reserve.config.index);
    let collateral_burned = user_state.get_collateral(reserve.config.index);
    let total_burned = supply_burned
        .checked_add(collateral_burned)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if total_burned <= 0 {
        panic_with_error!(e, PoolError::BadRequest);
    }
    let tokens_out = reserve.to_asset_from_b_token(e, total_burned);
    if tokens_out <= 0 {
        panic_with_error!(e, PoolError::InvalidBTokenBurnAmount);
    }
    let supply_tokens = reserve.to_asset_from_b_token(e, supply_burned);

    if supply_burned > 0 {
        user_state.remove_supply(e, &mut reserve, supply_burned);
    }
    if collateral_burned > 0 {
        user_state.remove_collateral(e, &mut reserve, collateral_burned);
    }
    reserve.require_utilization_below_100(e);
    pool.cache_reserve(reserve);

    let token = TokenClient::new(e, asset);
    let pool_address = e.current_contract_address();
    let balance_before = token.balance(&pool_address);
    token.transfer(&pool_address, user, &tokens_out);
    let balance_after = token.balance(&pool_address);
    if balance_before.checked_sub(balance_after) != Some(tokens_out) {
        panic_with_error!(e, PoolError::BalanceError);
    }

    pool.store_cached_reserves(e);
    user_state.store(e);
    (tokens_out, supply_burned, supply_tokens, collateral_burned)
}

#[cfg(test)]
mod tests {
    use sep_41_token::TokenClient;
    use soroban_sdk::{
        map,
        testutils::{Address as _, Ledger, LedgerInfo},
        Address, Env,
    };

    use crate::{
        pool::Positions,
        storage::{self, ReserveData},
        testutils::{
            self, create_pool_with_access_controller, MockAccessController,
            MockAccessControllerClient,
        },
        PoolClient,
    };

    struct Fixture {
        e: Env,
        pool: Address,
        asset: Address,
        user: Address,
        controller: Address,
    }

    fn fixture() -> Fixture {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            timestamp: 600,
            protocol_version: 27,
            sequence_number: 1234,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let controller = e.register(MockAccessController, ());
        let pool = create_pool_with_access_controller(&e, Some(controller.clone()));
        let admin = Address::generate(&e);
        let (asset, _) = testutils::create_token_contract(&e, &admin);
        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_supply = 150_0000000;
        reserve_data.d_supply = 0;
        reserve_data.last_time = e.ledger().timestamp();
        testutils::create_reserve(&e, &pool, &asset, &reserve_config, &reserve_data);

        let user = Address::generate(&e);
        let positions = Positions {
            liabilities: map![&e],
            collateral: map![&e, (0u32, 90_0000000i128)],
            supply: map![&e, (0u32, 60_0000000i128)],
        };
        e.as_contract(&pool, || storage::set_user_positions(&e, &user, &positions));

        Fixture {
            e,
            pool,
            asset,
            user,
            controller,
        }
    }

    fn reserve_data(fixture: &Fixture) -> ReserveData {
        fixture.e.as_contract(&fixture.pool, || {
            storage::get_res_data(&fixture.e, &fixture.asset)
        })
    }

    #[test]
    fn forced_withdrawal_returns_all_supply_only_to_target() {
        let fixture = fixture();
        let pool = PoolClient::new(&fixture.e, &fixture.pool);

        assert_eq!(
            pool.force_withdrawal(&fixture.user, &fixture.asset),
            150_0000000
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.user),
            150_0000000
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.pool),
            0
        );
        let positions = pool.get_positions(&fixture.user);
        assert!(positions.supply.is_empty());
        assert!(positions.collateral.is_empty());
        assert_eq!(reserve_data(&fixture).b_supply, 0);
    }

    #[test]
    fn forced_withdrawal_rejects_reauthorized_user() {
        let fixture = fixture();
        MockAccessControllerClient::new(&fixture.e, &fixture.controller).set_permissions(
            &fixture.pool,
            &fixture.user,
            &crate::access::RESERVE_SUPPLY_ALLOWED,
        );

        assert!(PoolClient::new(&fixture.e, &fixture.pool)
            .try_force_withdrawal(&fixture.user, &fixture.asset)
            .is_err());
        assert_eq!(reserve_data(&fixture).b_supply, 150_0000000);
    }
}
