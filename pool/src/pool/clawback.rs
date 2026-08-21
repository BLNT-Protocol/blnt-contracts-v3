use sep_41_token::{StellarAssetClient, TokenClient};
use soroban_sdk::{panic_with_error, Address, Env};

use crate::{storage, AuctionType, PoolError};

use super::{gulp::require_reconciled, Pool, User};

/// Claw back an exact underlying amount from a user's supplied reserve.
///
/// The reserve must be a Stellar Asset Contract whose balance entry for this
/// pool was created as clawbackable. The SAC enforces that condition and
/// requires its current administrator's authorization. Ordinary supply is
/// consumed before collateral, and collateral impairment deliberately does
/// not perform a health check so the inherited liquidation and bad-debt paths
/// can resolve any resulting shortfall. If collateral is consumed, any active
/// user-liquidation auction is invalidated because its position snapshot may
/// no longer be fillable.
pub fn execute_clawback(
    e: &Env,
    asset: &Address,
    from: &Address,
    amount: i128,
) -> (i128, i128, bool) {
    if amount <= 0 || from == &e.current_contract_address() {
        panic_with_error!(e, PoolError::BadRequest);
    }

    let mut pool = Pool::load(e);
    let mut reserve = pool.load_reserve(e, asset, true);
    require_reconciled(e, &reserve);
    let mut user = User::load(e, from);
    let reserve_index = reserve.config.index;
    let b_tokens = reserve.to_b_token_up(e, amount);
    let supply = user.get_supply(reserve_index);
    let collateral = user.get_collateral(reserve_index);
    let total = supply
        .checked_add(collateral)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if b_tokens <= 0 || b_tokens > total {
        panic_with_error!(e, PoolError::BalanceError);
    }

    let supply_burned = supply.min(b_tokens);
    let collateral_burned = b_tokens - supply_burned;
    if supply_burned > 0 {
        user.remove_supply(e, &mut reserve, supply_burned);
    }
    if collateral_burned > 0 {
        user.remove_collateral(e, &mut reserve, collateral_burned);
    }

    let pool_address = e.current_contract_address();
    let token = TokenClient::new(e, asset);
    let balance_before = token.balance(&pool_address);
    if balance_before < amount {
        panic_with_error!(e, PoolError::BalanceError);
    }

    // This is both the burn and the authoritative clawback-eligibility check.
    // For a SAC contract balance, the host rejects this call unless the entry
    // was created with clawback enabled, and it requires the SAC administrator.
    StellarAssetClient::new(e, asset).clawback(&pool_address, &amount);

    let expected_balance = balance_before
        .checked_sub(amount)
        .unwrap_or_else(|| panic_with_error!(e, PoolError::OverflowError));
    if token.balance(&pool_address) != expected_balance {
        panic_with_error!(e, PoolError::BalanceError);
    }

    let auction_invalidated = collateral_burned > 0
        && storage::has_auction(e, &(AuctionType::UserLiquidation as u32), from);
    if auction_invalidated {
        storage::del_auction(e, &(AuctionType::UserLiquidation as u32), from);
    }

    pool.cache_reserve(reserve);
    pool.store_cached_reserves(e);
    user.store(e);

    (supply_burned, collateral_burned, auction_invalidated)
}

#[cfg(test)]
mod tests {
    use sep_41_token::TokenClient;
    use soroban_sdk::{
        map,
        testutils::{Address as _, IssuerFlags, Ledger, LedgerInfo},
        token::StellarAssetClient,
        Address, Env, Map,
    };

    use crate::{
        auctions::AuctionData,
        pool::Positions,
        storage::{self, ReserveData},
        testutils, AuctionType, PoolClient,
    };

    struct TestFixture {
        e: Env,
        pool: Address,
        asset: Address,
        user: Address,
        admin: Address,
    }

    fn fixture(clawbackable: bool, pool_balance: i128) -> TestFixture {
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

        let pool = testutils::create_pool(&e);
        let admin = Address::generate(&e);
        let registered_asset = e.register_stellar_asset_contract_v2(admin.clone());
        if clawbackable {
            registered_asset
                .issuer()
                .set_flag(IssuerFlags::ClawbackEnabledFlag);
        }
        let asset = registered_asset.address();

        let (reserve_config, mut reserve_data) = testutils::default_reserve_meta();
        reserve_data.b_supply = 200_0000000;
        reserve_data.d_supply = reserve_data.b_supply - pool_balance;
        reserve_data.last_time = e.ledger().timestamp();
        testutils::create_reserve(&e, &pool, &asset, &reserve_config, &reserve_data);

        let user = Address::generate(&e);
        let mut liabilities = Map::new(&e);
        if reserve_data.d_supply > 0 {
            liabilities.set(0u32, reserve_data.d_supply);
        }
        let positions = Positions {
            liabilities,
            collateral: map![&e, (0u32, 90_0000000i128)],
            supply: map![&e, (0u32, 60_0000000i128)],
        };
        e.as_contract(&pool, || storage::set_user_positions(&e, &user, &positions));

        TestFixture {
            e,
            pool,
            asset,
            user,
            admin,
        }
    }

    fn reserve_data(fixture: &TestFixture) -> ReserveData {
        fixture.e.as_contract(&fixture.pool, || {
            storage::get_res_data(&fixture.e, &fixture.asset)
        })
    }

    fn has_liquidation(fixture: &TestFixture) -> bool {
        fixture.e.as_contract(&fixture.pool, || {
            storage::has_auction(
                &fixture.e,
                &(AuctionType::UserLiquidation as u32),
                &fixture.user,
            )
        })
    }

    fn set_liquidation(fixture: &TestFixture) {
        fixture.e.as_contract(&fixture.pool, || {
            storage::set_auction(
                &fixture.e,
                &(AuctionType::UserLiquidation as u32),
                &fixture.user,
                &AuctionData {
                    bid: map![&fixture.e],
                    lot: map![&fixture.e],
                    block: fixture.e.ledger().sequence() + 1,
                },
            );
        });
    }

    #[test]
    fn clawback_consumes_supply_before_collateral() {
        let fixture = fixture(true, 150_0000000);
        let pool_client = PoolClient::new(&fixture.e, &fixture.pool);

        pool_client
            .mock_all_auths_allowing_non_root_auth()
            .clawback(&fixture.asset, &fixture.user, &100_0000000);

        assert_eq!(fixture.e.auths().len(), 1);
        assert_eq!(fixture.e.auths().first().unwrap().0, fixture.admin);
        let positions = pool_client.get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), None);
        assert_eq!(positions.collateral.get(0), Some(50_0000000));
        assert_eq!(positions.liabilities.get(0), Some(50_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 100_0000000);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.pool),
            50_0000000
        );
    }

    #[test]
    fn clawback_requires_asset_admin_authorization() {
        let fixture = fixture(true, 150_0000000);
        let pool_client = PoolClient::new(&fixture.e, &fixture.pool);
        fixture.e.set_auths(&[]);

        assert!(pool_client
            .try_clawback(&fixture.asset, &fixture.user, &10_0000000)
            .is_err());

        let positions = pool_client.get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(60_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 200_0000000);
    }

    #[test]
    fn clawback_requires_clawbackable_pool_balance() {
        let fixture = fixture(false, 150_0000000);
        let pool_client = PoolClient::new(&fixture.e, &fixture.pool);

        assert!(pool_client
            .mock_all_auths_allowing_non_root_auth()
            .try_clawback(&fixture.asset, &fixture.user, &10_0000000)
            .is_err());

        let positions = pool_client.get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(60_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 200_0000000);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.pool),
            150_0000000
        );
    }

    #[test]
    fn clawback_requires_sufficient_pool_liquidity() {
        let fixture = fixture(true, 50_0000000);
        let pool_client = PoolClient::new(&fixture.e, &fixture.pool);

        assert!(pool_client
            .mock_all_auths_allowing_non_root_auth()
            .try_clawback(&fixture.asset, &fixture.user, &60_0000000)
            .is_err());

        let positions = pool_client.get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(60_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 200_0000000);
        assert_eq!(
            StellarAssetClient::new(&fixture.e, &fixture.asset).balance(&fixture.pool),
            50_0000000
        );
    }

    #[test]
    fn clawback_requires_sufficient_user_supply() {
        let fixture = fixture(true, 200_0000000);
        let pool_client = PoolClient::new(&fixture.e, &fixture.pool);

        assert!(pool_client
            .mock_all_auths_allowing_non_root_auth()
            .try_clawback(&fixture.asset, &fixture.user, &151_0000000)
            .is_err());

        let positions = pool_client.get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(60_0000000));
        assert_eq!(positions.collateral.get(0), Some(90_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 200_0000000);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.pool),
            200_0000000
        );
    }

    #[test]
    fn clawback_rounds_b_token_burn_up_and_burns_exact_assets() {
        let fixture = fixture(true, 150_0000000);
        fixture.e.as_contract(&fixture.pool, || {
            let mut data = storage::get_res_data(&fixture.e, &fixture.asset);
            data.b_rate = 1_500_000_000_000;
            storage::set_res_data(&fixture.e, &fixture.asset, &data);
        });
        StellarAssetClient::new(&fixture.e, &fixture.asset)
            .mock_all_auths()
            .mint(&fixture.pool, &100_0000000);

        PoolClient::new(&fixture.e, &fixture.pool)
            .mock_all_auths_allowing_non_root_auth()
            .clawback(&fixture.asset, &fixture.user, &10_0000001);

        let positions = PoolClient::new(&fixture.e, &fixture.pool).get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(53_3333332));
        assert_eq!(positions.collateral.get(0), Some(90_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 193_3333332);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.pool),
            239_9999999
        );
    }

    #[test]
    fn clawback_preserves_an_unaffected_user_liquidation() {
        let fixture = fixture(true, 150_0000000);
        set_liquidation(&fixture);

        PoolClient::new(&fixture.e, &fixture.pool)
            .mock_all_auths_allowing_non_root_auth()
            .clawback(&fixture.asset, &fixture.user, &10_0000000);

        let positions = PoolClient::new(&fixture.e, &fixture.pool).get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(50_0000000));
        assert_eq!(positions.collateral.get(0), Some(90_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 190_0000000);
        assert!(has_liquidation(&fixture));
    }

    #[test]
    fn clawback_invalidates_an_affected_user_liquidation() {
        let fixture = fixture(true, 150_0000000);
        set_liquidation(&fixture);

        PoolClient::new(&fixture.e, &fixture.pool)
            .mock_all_auths_allowing_non_root_auth()
            .clawback(&fixture.asset, &fixture.user, &70_0000000);

        let positions = PoolClient::new(&fixture.e, &fixture.pool).get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), None);
        assert_eq!(positions.collateral.get(0), Some(80_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 130_0000000);
        assert!(!has_liquidation(&fixture));
    }

    #[test]
    fn failed_clawback_preserves_an_affected_user_liquidation() {
        let fixture = fixture(false, 150_0000000);
        set_liquidation(&fixture);

        assert!(PoolClient::new(&fixture.e, &fixture.pool)
            .mock_all_auths_allowing_non_root_auth()
            .try_clawback(&fixture.asset, &fixture.user, &70_0000000)
            .is_err());

        let positions = PoolClient::new(&fixture.e, &fixture.pool).get_positions(&fixture.user);
        assert_eq!(positions.supply.get(0), Some(60_0000000));
        assert_eq!(positions.collateral.get(0), Some(90_0000000));
        assert_eq!(reserve_data(&fixture).b_supply, 200_0000000);
        assert!(has_liquidation(&fixture));
    }
}
