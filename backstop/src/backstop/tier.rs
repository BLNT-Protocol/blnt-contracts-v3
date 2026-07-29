use soroban_sdk::{contracttype, panic_with_error, Address, Env};

use crate::{errors::BackstopError, storage};

use super::{PoolBalance, UserBalance};

/// The fixed v3 backstop assets in first-loss order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopTier {
    BlndUsdc,
    BlndXlm,
    Usdc,
}

/// The complete accounting state for one pool and backstop tier.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct PoolTierState {
    pub assets: i128,
    pub queued_shares: i128,
    pub shares: i128,
}

/// Aggregate accounting totals for one backstop tier.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[contracttype]
pub struct TierTotals {
    pub assets: i128,
    pub queued_shares: i128,
    pub shares: i128,
}

pub fn token(e: &Env, tier: BackstopTier) -> Address {
    match tier {
        BackstopTier::BlndUsdc => storage::get_blnd_usdc_token(e),
        BackstopTier::BlndXlm => storage::get_blnd_xlm_token(e),
        BackstopTier::Usdc => storage::get_usdc_token(e),
    }
}

pub fn pool_state(e: &Env, tier: BackstopTier, pool: &Address) -> PoolTierState {
    let balance = storage::get_pool_balance_for_tier(e, tier, pool);
    PoolTierState {
        assets: balance.tokens,
        queued_shares: balance.q4w,
        shares: balance.shares,
    }
}

pub fn user_total_shares(balance: &UserBalance) -> i128 {
    let mut total = balance.shares;
    for entry in balance.q4w.iter() {
        total += entry.amount;
    }
    total
}

pub fn user_queued_shares(balance: &UserBalance) -> i128 {
    let mut total = 0;
    for entry in balance.q4w.iter() {
        total += entry.amount;
    }
    total
}

pub fn preview_deposit(pool_balance: &PoolBalance, assets: i128) -> i128 {
    pool_balance.convert_to_shares(assets)
}

pub fn preview_withdrawal(pool_balance: &PoolBalance, shares: i128) -> i128 {
    pool_balance.convert_to_tokens(shares)
}

pub fn update_totals(
    e: &Env,
    tier: BackstopTier,
    asset_delta: i128,
    share_delta: i128,
    queued_share_delta: i128,
) {
    let mut totals = storage::get_tier_totals(e, tier);
    totals.assets = totals
        .assets
        .checked_add(asset_delta)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    totals.shares = totals
        .shares
        .checked_add(share_delta)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    totals.queued_shares = totals
        .queued_shares
        .checked_add(queued_share_delta)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if totals.assets < 0
        || totals.shares < 0
        || totals.queued_shares < 0
        || totals.queued_shares > totals.shares
    {
        panic_with_error!(e, BackstopError::InternalError);
    }
    storage::set_tier_totals(e, tier, &totals);
}

#[cfg(test)]
mod tests {
    use mock_pool::{MockPoolClient, Positions};
    use soroban_sdk::{
        map,
        testutils::{Address as _, Ledger, LedgerInfo},
        Address,
    };

    use crate::{
        constants::{MAX_Q4W_SIZE, Q4W_LOCK_TIME},
        testutils::{
            create_backstop, create_backstop_token, create_blnd_xlm_token,
            create_mock_pool_factory, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    #[test]
    fn three_tiers_keep_pool_and_user_accounting_isolated() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (blnd_usdc, blnd_usdc_client) = create_backstop_token(&e, &backstop, &admin);
        let (blnd_xlm, blnd_xlm_client) = create_blnd_xlm_token(&e, &backstop, &admin);
        let (usdc, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        blnd_usdc_client.mint(&user, &100);
        blnd_xlm_client.mint(&user, &200);
        usdc_client.mint(&user, &300);

        let client = BackstopClient::new(&e, &backstop);
        assert_eq!(client.deposit_blnd_usdc(&user, &pool, &100), 100);
        assert_eq!(client.deposit_blnd_xlm(&user, &pool, &200), 200);
        assert_eq!(client.deposit_usdc(&user, &pool, &300), 300);

        assert_eq!(client.tier_token(&BackstopTier::BlndUsdc), blnd_usdc);
        assert_eq!(client.tier_token(&BackstopTier::BlndXlm), blnd_xlm);
        assert_eq!(client.tier_token(&BackstopTier::Usdc), usdc);
        assert_eq!(
            client.pool_tier_state(&BackstopTier::BlndUsdc, &pool),
            PoolTierState {
                assets: 100,
                queued_shares: 0,
                shares: 100,
            }
        );
        assert_eq!(
            client.pool_tier_state(&BackstopTier::BlndXlm, &pool),
            PoolTierState {
                assets: 200,
                queued_shares: 0,
                shares: 200,
            }
        );
        assert_eq!(
            client.pool_tier_state(&BackstopTier::Usdc, &pool),
            PoolTierState {
                assets: 300,
                queued_shares: 0,
                shares: 300,
            }
        );
        assert_eq!(
            client.tier_totals(&BackstopTier::BlndXlm),
            TierTotals {
                assets: 200,
                queued_shares: 0,
                shares: 200,
            }
        );
        assert_eq!(client.tier_shares(&BackstopTier::Usdc, &user, &pool), 300);
        assert_eq!(
            client.tier_active_shares(&BackstopTier::Usdc, &user, &pool),
            300
        );
        assert_eq!(
            client.tier_queued_shares(&BackstopTier::Usdc, &user, &pool),
            0
        );
    }

    #[test]
    fn q4w_limit_is_aggregate_and_withdrawal_is_tier_specific() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            protocol_version: 27,
            sequence_number: 1,
            timestamp: 10_000,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let recipient = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (_, blnd_usdc_client) = create_backstop_token(&e, &backstop, &admin);
        let (_, blnd_xlm_client) = create_blnd_xlm_token(&e, &backstop, &admin);
        let (_, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_mock_pool(&pool);

        blnd_usdc_client.mint(&user, &100);
        blnd_xlm_client.mint(&user, &100);
        usdc_client.mint(&user, &100);
        let client = BackstopClient::new(&e, &backstop);
        client.deposit_blnd_usdc(&user, &pool, &100);
        client.deposit_blnd_xlm(&user, &pool, &100);
        client.deposit_usdc(&user, &pool, &100);

        for _ in 0..10 {
            client.queue_blnd_usdc_withdrawal(&user, &pool, &1);
        }
        for _ in 0..5 {
            client.queue_blnd_xlm_withdrawal(&user, &pool, &1);
            client.queue_usdc_withdrawal(&user, &pool, &1);
        }
        assert_eq!(
            client
                .tier_withdrawal_queue(&BackstopTier::BlndUsdc, &user, &pool)
                .len()
                + client
                    .tier_withdrawal_queue(&BackstopTier::BlndXlm, &user, &pool)
                    .len()
                + client
                    .tier_withdrawal_queue(&BackstopTier::Usdc, &user, &pool)
                    .len(),
            MAX_Q4W_SIZE
        );
        assert!(client.try_queue_usdc_withdrawal(&user, &pool, &1).is_err());

        client.dequeue_blnd_usdc_withdrawal(&user, &pool, &1);
        client.queue_blnd_xlm_withdrawal(&user, &pool, &1);
        e.ledger().set_timestamp(10_000 + Q4W_LOCK_TIME + 1);
        assert_eq!(client.withdraw_blnd_xlm(&user, &pool, &6, &recipient), 6);
        assert_eq!(blnd_xlm_client.balance(&recipient), 6);
        assert_eq!(
            client.tier_active_shares(&BackstopTier::BlndXlm, &user, &pool),
            94
        );
        assert_eq!(
            client.tier_queued_shares(&BackstopTier::BlndXlm, &user, &pool),
            0
        );
        assert_eq!(
            client.tier_totals(&BackstopTier::BlndXlm),
            TierTotals {
                assets: 94,
                queued_shares: 0,
                shares: 94,
            }
        );
    }

    #[test]
    fn deposit_requires_callback_interface_but_not_a_true_callback_result() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let incompatible_pool = Address::generate(&e);
        let compatible_pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (_, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        let client = BackstopClient::new(&e, &backstop);

        usdc_client.mint(&user, &200);
        factory.set_pool(&incompatible_pool);
        assert!(client
            .try_deposit_usdc(&user, &incompatible_pool, &100)
            .is_err());
        assert_eq!(usdc_client.balance(&user), 200);
        assert_eq!(
            client.pool_tier_state(&BackstopTier::Usdc, &incompatible_pool),
            PoolTierState {
                assets: 0,
                queued_shares: 0,
                shares: 0,
            }
        );

        factory.set_mock_pool(&compatible_pool);
        let pool_client = MockPoolClient::new(&e, &compatible_pool);
        pool_client.set_positions(
            &backstop,
            &Positions {
                liabilities: map![&e, (0, 1)],
                collateral: map![&e],
                supply: map![&e],
            },
        );
        assert!(!pool_client.backstop_withdrawal_allowed(&backstop));
        assert_eq!(client.deposit_usdc(&user, &compatible_pool, &100), 100);
        assert_eq!(usdc_client.balance(&user), 100);
    }
}
