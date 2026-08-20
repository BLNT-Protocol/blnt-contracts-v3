use sep_41_token::{StellarAssetClient, TokenClient};
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{panic_with_error, unwrap::UnwrapOptimized, Address, Env};

use crate::{emissions, storage, BackstopError};

use super::{is_blnd_emission_tier, tier_token, BackstopTier, PoolBalance, UserBalance};

/// Claw back an exact underlying amount from one user's pool-tier position.
///
/// The configured tier token must be a Stellar Asset Contract whose balance
/// entry for this backstop was created as clawbackable. Active shares are
/// consumed before queued shares; queued shares are consumed newest first.
pub fn execute_clawback(
    e: &Env,
    tier: BackstopTier,
    pool: &Address,
    from: &Address,
    amount: i128,
) -> (i128, i128) {
    if amount <= 0 || from == pool || from == &e.current_contract_address() {
        panic_with_error!(e, BackstopError::BadRequest);
    }

    let token_address = tier_token(e, pool, tier);
    let mut pool_balance = storage::get_pool_balance_for_tier(e, tier, pool);
    if amount > pool_balance.tokens {
        panic_with_error!(e, BackstopError::BalanceError);
    }
    let shares_to_burn = shares_for_tokens(e, &pool_balance, amount);
    if shares_to_burn <= 0 {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    let mut user_balance = storage::get_user_balance_for_tier(e, tier, pool, from);
    let (active_shares_burned, q4w_shares_burned) =
        remove_user_shares(e, &mut user_balance, shares_to_burn);

    let emission_eligible = is_blnd_emission_tier(e, pool, tier);
    emissions::checkpoint_user_ongoing_for_weight_change(e, tier, from, pool, emission_eligible);

    pool_balance.tokens = pool_balance
        .tokens
        .checked_sub(amount)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    pool_balance.shares = pool_balance
        .shares
        .checked_sub(shares_to_burn)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    pool_balance.q4w = pool_balance
        .q4w
        .checked_sub(q4w_shares_burned)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));

    let backstop = e.current_contract_address();
    let token = TokenClient::new(e, &token_address);
    let balance_before = token.balance(&backstop);
    if balance_before < amount {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    // The SAC invocation is both the burn and the authoritative check that
    // this backstop balance is clawbackable and the current issuer authorized.
    StellarAssetClient::new(e, &token_address).clawback(&backstop, &amount);

    let expected_balance = balance_before
        .checked_sub(amount)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if token.balance(&backstop) != expected_balance {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    storage::set_user_balance_for_tier(e, tier, pool, from, &user_balance);
    storage::set_pool_balance_for_tier(e, tier, pool, &pool_balance);
    emissions::finish_pool_weight_change(e, pool, emission_eligible);

    (active_shares_burned, q4w_shares_burned)
}

fn shares_for_tokens(e: &Env, pool: &PoolBalance, tokens: i128) -> i128 {
    if pool.tokens <= 0 || pool.shares <= 0 || tokens > pool.tokens {
        panic_with_error!(e, BackstopError::BalanceError);
    }
    if tokens == pool.tokens {
        return pool.shares;
    }
    tokens
        .fixed_mul_ceil(pool.shares, pool.tokens)
        .unwrap_optimized()
}

fn remove_user_shares(e: &Env, user: &mut UserBalance, shares: i128) -> (i128, i128) {
    let mut queued = 0_i128;
    for entry in user.q4w.iter() {
        queued = queued
            .checked_add(entry.amount)
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    }
    let total = user
        .shares
        .checked_add(queued)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if shares <= 0 || shares > total {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    let active_shares_burned = user.shares.min(shares);
    user.shares = user
        .shares
        .checked_sub(active_shares_burned)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let q4w_shares_burned = shares
        .checked_sub(active_shares_burned)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if q4w_shares_burned > 0 {
        user.dequeue_shares(e, q4w_shares_burned);
    }

    (active_shares_burned, q4w_shares_burned)
}

#[cfg(test)]
mod tests {
    use mock_emitter::MockEmitter;
    use sep_41_token::{StellarAssetClient, TokenClient};
    use soroban_sdk::{
        testutils::{Address as _, IssuerFlags},
        vec, Address, Env,
    };

    use crate::{
        dependencies::{BackstopTierConfig, EmitterClient, FactoryBackstopAsset},
        storage,
        testutils::{create_backstop, create_mock_pool_factory, sync_mock_pool_factory_config},
        BackstopClient,
    };

    use super::*;

    struct TestFixture {
        e: Env,
        backstop: Address,
        pool: Address,
        user: Address,
        admin: Address,
        asset: Address,
    }

    fn fixture(clawbackable: bool, deposit: i128) -> TestFixture {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();

        let backstop = create_backstop(&e);
        let pool = Address::generate(&e);
        let user = Address::generate(&e);
        let admin = Address::generate(&e);
        let registered_asset = e.register_stellar_asset_contract_v2(admin.clone());
        if clawbackable {
            registered_asset
                .issuer()
                .set_flag(IssuerFlags::ClawbackEnabledFlag);
        }
        let asset = registered_asset.address();

        e.as_contract(&backstop, || storage::set_usdc_token(&e, &asset));
        sync_mock_pool_factory_config(&e, &backstop);
        create_mock_pool_factory(&e, &backstop).1.set_pool(&pool);

        StellarAssetClient::new(&e, &asset).mint(&user, &deposit);
        BackstopClient::new(&e, &backstop).deposit(
            &BackstopTier::ThirdLoss,
            &user,
            &pool,
            &deposit,
        );

        TestFixture {
            e,
            backstop,
            pool,
            user,
            admin,
            asset,
        }
    }

    fn user_balance(fixture: &TestFixture) -> UserBalance {
        BackstopClient::new(&fixture.e, &fixture.backstop).user_balance(
            &BackstopTier::ThirdLoss,
            &fixture.pool,
            &fixture.user,
        )
    }

    fn pool_balance(fixture: &TestFixture) -> PoolBalance {
        fixture.e.as_contract(&fixture.backstop, || {
            storage::get_pool_balance_for_tier(&fixture.e, BackstopTier::ThirdLoss, &fixture.pool)
        })
    }

    #[test]
    fn clawback_consumes_active_then_newest_queued_shares() {
        let fixture = fixture(true, 100_0000000);
        let client = BackstopClient::new(&fixture.e, &fixture.backstop);
        client.queue_withdrawal(
            &BackstopTier::ThirdLoss,
            &fixture.user,
            &fixture.pool,
            &20_0000000,
        );
        client.queue_withdrawal(
            &BackstopTier::ThirdLoss,
            &fixture.user,
            &fixture.pool,
            &30_0000000,
        );

        client.clawback(
            &BackstopTier::ThirdLoss,
            &fixture.pool,
            &fixture.user,
            &75_0000000,
        );

        assert_eq!(fixture.e.auths().len(), 1);
        assert_eq!(fixture.e.auths().first().unwrap().0, fixture.admin);
        let user = user_balance(&fixture);
        assert_eq!(user.shares, 0);
        assert_eq!(user.q4w.len(), 2);
        assert_eq!(user.q4w.get_unchecked(0).amount, 20_0000000);
        assert_eq!(user.q4w.get_unchecked(1).amount, 5_0000000);
        let pool = pool_balance(&fixture);
        assert_eq!(pool.tokens, 25_0000000);
        assert_eq!(pool.shares, 25_0000000);
        assert_eq!(pool.q4w, 25_0000000);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.backstop),
            25_0000000
        );
    }

    #[test]
    fn clawback_requires_asset_admin_authorization() {
        let fixture = fixture(true, 100_0000000);
        fixture.e.set_auths(&[]);

        assert!(BackstopClient::new(&fixture.e, &fixture.backstop)
            .try_clawback(
                &BackstopTier::ThirdLoss,
                &fixture.pool,
                &fixture.user,
                &10_0000000,
            )
            .is_err());

        assert_eq!(user_balance(&fixture).shares, 100_0000000);
        assert_eq!(pool_balance(&fixture).tokens, 100_0000000);
    }

    #[test]
    fn clawback_requires_clawbackable_backstop_balance() {
        let fixture = fixture(false, 100_0000000);
        let client = BackstopClient::new(&fixture.e, &fixture.backstop);
        client.queue_withdrawal(
            &BackstopTier::ThirdLoss,
            &fixture.user,
            &fixture.pool,
            &40_0000000,
        );

        assert!(client
            .try_clawback(
                &BackstopTier::ThirdLoss,
                &fixture.pool,
                &fixture.user,
                &70_0000000,
            )
            .is_err());

        let user = user_balance(&fixture);
        assert_eq!(user.shares, 60_0000000);
        assert_eq!(user.q4w.get_unchecked(0).amount, 40_0000000);
        let pool = pool_balance(&fixture);
        assert_eq!(pool.tokens, 100_0000000);
        assert_eq!(pool.shares, 100_0000000);
        assert_eq!(pool.q4w, 40_0000000);
    }

    #[test]
    fn clawback_requires_sufficient_user_shares() {
        let fixture = fixture(true, 100_0000000);
        let other = Address::generate(&fixture.e);
        StellarAssetClient::new(&fixture.e, &fixture.asset).mint(&other, &100_0000000);
        BackstopClient::new(&fixture.e, &fixture.backstop).deposit(
            &BackstopTier::ThirdLoss,
            &other,
            &fixture.pool,
            &100_0000000,
        );

        assert!(BackstopClient::new(&fixture.e, &fixture.backstop)
            .try_clawback(
                &BackstopTier::ThirdLoss,
                &fixture.pool,
                &fixture.user,
                &101_0000000,
            )
            .is_err());

        assert_eq!(user_balance(&fixture).shares, 100_0000000);
        assert_eq!(pool_balance(&fixture).tokens, 200_0000000);
    }

    #[test]
    fn clawback_rounds_share_burn_up_and_burns_exact_assets() {
        let fixture = fixture(true, 100_0000000);
        StellarAssetClient::new(&fixture.e, &fixture.asset).mint(&fixture.backstop, &50_0000000);
        fixture.e.as_contract(&fixture.backstop, || {
            let mut pool = storage::get_pool_balance_for_tier(
                &fixture.e,
                BackstopTier::ThirdLoss,
                &fixture.pool,
            );
            pool.tokens += 50_0000000;
            storage::set_pool_balance_for_tier(
                &fixture.e,
                BackstopTier::ThirdLoss,
                &fixture.pool,
                &pool,
            );
        });

        BackstopClient::new(&fixture.e, &fixture.backstop).clawback(
            &BackstopTier::ThirdLoss,
            &fixture.pool,
            &fixture.user,
            &10_0000000,
        );

        assert_eq!(user_balance(&fixture).shares, 93_3333333);
        let pool = pool_balance(&fixture);
        assert_eq!(pool.tokens, 140_0000000);
        assert_eq!(pool.shares, 93_3333333);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.backstop),
            140_0000000
        );
    }

    #[test]
    fn clawback_requires_sufficient_physical_balance() {
        let fixture = fixture(true, 100_0000000);
        StellarAssetClient::new(&fixture.e, &fixture.asset)
            .clawback(&fixture.backstop, &50_0000000);

        assert!(BackstopClient::new(&fixture.e, &fixture.backstop)
            .try_clawback(
                &BackstopTier::ThirdLoss,
                &fixture.pool,
                &fixture.user,
                &60_0000000,
            )
            .is_err());

        assert_eq!(user_balance(&fixture).shares, 100_0000000);
        assert_eq!(pool_balance(&fixture).tokens, 100_0000000);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.asset).balance(&fixture.backstop),
            50_0000000
        );
    }

    #[test]
    fn clawback_remains_available_during_migration_transition() {
        let fixture = fixture(true, 100_0000000);
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_blnd_usdc_token(&fixture.e, &fixture.asset);
            storage::set_pool_backstop_config(
                &fixture.e,
                &fixture.pool,
                &vec![
                    &fixture.e,
                    BackstopTierConfig {
                        asset: FactoryBackstopAsset::Xlm,
                        take_rate_weight: 3,
                    },
                    BackstopTierConfig {
                        asset: FactoryBackstopAsset::Usdc,
                        take_rate_weight: 2,
                    },
                    BackstopTierConfig {
                        asset: FactoryBackstopAsset::BlndUsdc,
                        take_rate_weight: 1,
                    },
                ],
            );
        });

        let blnd = fixture
            .e
            .as_contract(&fixture.backstop, || storage::get_blnd_token(&fixture.e));
        let emitter = fixture.e.register(MockEmitter, ());
        EmitterClient::new(&fixture.e, &emitter).initialize(
            &blnd,
            &fixture.backstop,
            &fixture.asset,
        );
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_emitter(&fixture.e, &emitter);
        });

        let client = BackstopClient::new(&fixture.e, &fixture.backstop);
        assert!(client
            .try_queue_withdrawal(
                &BackstopTier::ThirdLoss,
                &fixture.user,
                &fixture.pool,
                &10_0000000,
            )
            .is_err());
        client.clawback(
            &BackstopTier::ThirdLoss,
            &fixture.pool,
            &fixture.user,
            &10_0000000,
        );

        assert_eq!(user_balance(&fixture).shares, 90_0000000);
        assert_eq!(pool_balance(&fixture).tokens, 90_0000000);
    }
}
