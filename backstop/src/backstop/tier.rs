use soroban_sdk::{contracttype, Address, Env};

use crate::storage;

/// The fixed v3 backstop asset identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum BackstopTier {
    BlndUsdc,
    BlndXlm,
    Usdc,
}

pub fn token(e: &Env, tier: BackstopTier) -> Address {
    match tier {
        BackstopTier::BlndUsdc => storage::get_blnd_usdc_token(e),
        BackstopTier::BlndXlm => storage::get_blnd_xlm_token(e),
        BackstopTier::Usdc => storage::get_usdc_token(e),
    }
}

#[cfg(test)]
mod tests {
    use soroban_sdk::{
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
        assert_eq!(
            client.deposit(&crate::BackstopTier::BlndUsdc, &user, &pool, &100),
            100
        );
        assert_eq!(
            client.deposit(&crate::BackstopTier::BlndXlm, &user, &pool, &200),
            200
        );
        assert_eq!(
            client.deposit(&crate::BackstopTier::Usdc, &user, &pool, &300),
            300
        );

        assert_eq!(client.backstop_token(&BackstopTier::BlndUsdc), blnd_usdc);
        assert_eq!(client.backstop_token(&BackstopTier::BlndXlm), blnd_xlm);
        assert_eq!(client.backstop_token(&BackstopTier::Usdc), usdc);
        let pool_data = client.pool_data(&pool);
        assert_eq!(pool_data.blnd_usdc.assets, 100);
        assert_eq!(pool_data.blnd_usdc.shares, 100);
        assert_eq!(pool_data.blnd_usdc.queued_shares, 0);
        assert_eq!(pool_data.blnd_xlm.assets, 200);
        assert_eq!(pool_data.blnd_xlm.shares, 200);
        assert_eq!(pool_data.blnd_xlm.queued_shares, 0);
        assert_eq!(pool_data.usdc.assets, 300);
        assert_eq!(pool_data.usdc.shares, 300);
        assert_eq!(pool_data.usdc.queued_shares, 0);
        let user_balance = client.user_balance(&BackstopTier::Usdc, &pool, &user);
        assert_eq!(user_balance.shares, 300);
        assert!(user_balance.q4w.is_empty());
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
        client.deposit(&crate::BackstopTier::BlndUsdc, &user, &pool, &100);
        client.deposit(&crate::BackstopTier::BlndXlm, &user, &pool, &100);
        client.deposit(&crate::BackstopTier::Usdc, &user, &pool, &100);

        for _ in 0..10 {
            client.queue_withdrawal(&crate::BackstopTier::BlndUsdc, &user, &pool, &1);
        }
        for _ in 0..5 {
            client.queue_withdrawal(&crate::BackstopTier::BlndXlm, &user, &pool, &1);
            client.queue_withdrawal(&crate::BackstopTier::Usdc, &user, &pool, &1);
        }
        assert_eq!(
            client
                .user_balance(&BackstopTier::BlndUsdc, &pool, &user)
                .q4w
                .len()
                + client
                    .user_balance(&BackstopTier::BlndXlm, &pool, &user)
                    .q4w
                    .len()
                + client
                    .user_balance(&BackstopTier::Usdc, &pool, &user)
                    .q4w
                    .len(),
            MAX_Q4W_SIZE
        );
        assert!(client
            .try_queue_withdrawal(&crate::BackstopTier::Usdc, &user, &pool, &1)
            .is_err());

        client.dequeue_withdrawal(&crate::BackstopTier::BlndUsdc, &user, &pool, &1);
        client.queue_withdrawal(&crate::BackstopTier::BlndXlm, &user, &pool, &1);
        e.ledger().set_timestamp(10_000 + Q4W_LOCK_TIME + 1);
        assert_eq!(
            client.withdraw(&crate::BackstopTier::BlndXlm, &user, &pool, &6, &recipient),
            6
        );
        assert_eq!(blnd_xlm_client.balance(&recipient), 6);
        let user_balance = client.user_balance(&BackstopTier::BlndXlm, &pool, &user);
        assert_eq!(user_balance.shares, 94);
        assert!(user_balance.q4w.is_empty());
    }

    #[test]
    fn deposit_requires_factory_registration_only() {
        let e = Env::default();
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let pool = Address::generate(&e);
        let backstop = create_backstop(&e);
        let (_, usdc_client) = create_usdc_token(&e, &backstop, &admin);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        let client = BackstopClient::new(&e, &backstop);

        usdc_client.mint(&user, &200);
        assert!(client
            .try_deposit(&crate::BackstopTier::Usdc, &user, &pool, &100)
            .is_err());
        assert_eq!(usdc_client.balance(&user), 200);

        factory.set_pool(&pool);
        assert_eq!(
            client.deposit(&crate::BackstopTier::Usdc, &user, &pool, &100),
            100
        );
        assert_eq!(usdc_client.balance(&user), 100);
    }
}
