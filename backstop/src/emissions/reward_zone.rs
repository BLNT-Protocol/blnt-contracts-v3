use soroban_sdk::{contracttype, panic_with_error, Address, Env, Vec};

use crate::{
    backstop::{build_pool_valuation, quote_activation},
    constants::MAX_RZ_SIZE,
    errors::BackstopError,
    migration, storage,
};

use super::{
    policy::{eligible_blnd, pool_spot_blnd_emission_values},
    refresh_pool_ongoing_assets,
};

const CHECKPOINT_MAX_AGE_SECONDS: u64 = 60 * 60;

/// Most recent completed distribution checkpoint used to guard membership edits.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct RewardZoneCheckpoint {
    pub timestamp: u64,
}

pub(crate) fn get_reward_zone(e: &Env) -> Vec<Address> {
    let reward_zone = storage::get_reward_zone(e);
    if reward_zone.len() > MAX_RZ_SIZE {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    reward_zone
}

pub(crate) fn get_reward_zone_checkpoint(e: &Env) -> Option<RewardZoneCheckpoint> {
    storage::get_reward_zone_checkpoint(e).map(|timestamp| RewardZoneCheckpoint { timestamp })
}

pub(crate) fn add_to_reward_zone(
    e: &Env,
    to_add: &Address,
    to_remove: Option<&Address>,
) -> Option<Address> {
    migration::require_weight_mutation_allowed(e);
    let mut reward_zone = get_reward_zone(e);
    if reward_zone.contains(to_add.clone()) {
        panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
    }

    let valuation = build_pool_valuation(e, to_add);
    if !quote_activation(e, &valuation.active_values, false).meets_threshold {
        panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
    }
    let entrant_weight = eligible_blnd(e, &pool_spot_blnd_emission_values(e, to_add));
    if entrant_weight <= 0 {
        panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
    }
    if !reward_zone.is_empty() {
        require_recent_distribution_checkpoint(e);
    }

    let removed = if reward_zone.len() < MAX_RZ_SIZE {
        None
    } else {
        let to_remove =
            to_remove.unwrap_or_else(|| panic_with_error!(e, BackstopError::RewardZoneFull));
        let remove_index = reward_zone
            .first_index_of(to_remove.clone())
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidRewardZoneEntry));
        let removed_weight = eligible_blnd(e, &pool_spot_blnd_emission_values(e, to_remove));
        if entrant_weight <= removed_weight {
            panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
        }
        reward_zone.remove(remove_index);
        Some(to_remove.clone())
    };

    reward_zone.push_front(to_add.clone());
    refresh_pool_ongoing_assets(e, to_add);
    set_reward_zone(e, &reward_zone);
    removed
}

pub(crate) fn remove_from_reward_zone(e: &Env, to_remove: &Address) {
    migration::require_weight_mutation_allowed(e);
    let mut reward_zone = get_reward_zone(e);
    let remove_index = reward_zone
        .first_index_of(to_remove.clone())
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidRewardZoneEntry));
    let removed_weight = eligible_blnd(e, &pool_spot_blnd_emission_values(e, to_remove));
    if removed_weight > 0 {
        let valuation = build_pool_valuation(e, to_remove);
        if quote_activation(e, &valuation.active_values, true).meets_threshold {
            panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
        }
        require_recent_distribution_checkpoint(e);
    }

    reward_zone.remove(remove_index);
    set_reward_zone(e, &reward_zone);
}

fn set_reward_zone(e: &Env, reward_zone: &Vec<Address>) {
    if reward_zone.len() > MAX_RZ_SIZE {
        panic_with_error!(e, BackstopError::RewardZoneFull);
    }
    storage::set_reward_zone(e, reward_zone);
}

fn require_recent_distribution_checkpoint(e: &Env) {
    if !storage::get_reward_zone_distribution_started(e) {
        return;
    }
    let now = e.ledger().timestamp();
    let checkpoint = get_reward_zone_checkpoint(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::DistributionCheckpointRequired))
        .timestamp;
    if checkpoint > now
        || now
            .checked_sub(checkpoint)
            .is_none_or(|age| age > CHECKPOINT_MAX_AGE_SECONDS)
    {
        panic_with_error!(e, BackstopError::DistributionCheckpointRequired);
    }
}

#[cfg(test)]
mod tests {
    use mock_pool_factory::MockPoolFactoryClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env,
    };

    use crate::{
        backstop::{BackstopTier, PoolBalance},
        constants::{
            ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC, SCALAR_7,
        },
        storage,
        testutils::{
            create_backstop, create_blnd_token, create_comet_lp_pool, create_mock_pool_factory,
            create_token, create_usdc_token,
        },
        BackstopClient,
    };

    use super::{CHECKPOINT_MAX_AGE_SECONDS, MAX_RZ_SIZE};

    struct Fixture {
        backstop: Address,
        e: Env,
        factory: Address,
    }

    impl Fixture {
        fn create() -> Self {
            let e = Env::default();
            e.mock_all_auths();
            e.ledger().set_timestamp(10_000);
            let admin = Address::generate(&e);
            let backstop = create_backstop(&e);
            let (blnd, _) = create_blnd_token(&e, &backstop, &admin);
            let (usdc, _) = create_usdc_token(&e, &backstop, &admin);
            let (xlm, _) = create_token(&e, &admin);
            let (blnd_usdc, _) = create_comet_lp_pool(&e, &admin, &blnd, &usdc);
            let (blnd_xlm, _) = create_comet_lp_pool(&e, &admin, &blnd, &xlm);
            let (factory, _) = create_mock_pool_factory(&e, &backstop);
            e.as_contract(&backstop, || {
                storage::set_blnd_usdc_token(&e, &blnd_usdc);
                storage::set_blnd_xlm_token(&e, &blnd_xlm);
            });
            Self {
                backstop,
                e,
                factory,
            }
        }

        fn client(&self) -> BackstopClient<'_> {
            BackstopClient::new(&self.e, &self.backstop)
        }

        fn pool(&self, blnd_usdc: i128, queued_blnd_usdc: i128, usdc: i128) -> Address {
            let pool = Address::generate(&self.e);
            MockPoolFactoryClient::new(&self.e, &self.factory).set_mock_pool(&pool);
            self.e.as_contract(&self.backstop, || {
                storage::set_pool_balance_for_tier(
                    &self.e,
                    BackstopTier::BlndUsdc,
                    &pool,
                    &PoolBalance {
                        q4w: queued_blnd_usdc,
                        shares: blnd_usdc,
                        tokens: blnd_usdc,
                    },
                );
                storage::set_pool_balance_for_tier(
                    &self.e,
                    BackstopTier::Usdc,
                    &pool,
                    &PoolBalance {
                        q4w: 0,
                        shares: usdc,
                        tokens: usdc,
                    },
                );
            });
            pool
        }

        fn set_pool_tier(
            &self,
            pool: &Address,
            tier: BackstopTier,
            assets: i128,
            queued_shares: i128,
        ) {
            self.e.as_contract(&self.backstop, || {
                storage::set_pool_balance_for_tier(
                    &self.e,
                    tier,
                    pool,
                    &PoolBalance {
                        q4w: queued_shares,
                        shares: assets,
                        tokens: assets,
                    },
                );
            });
        }

        fn checkpoint(&self, timestamp: u64) {
            self.e.as_contract(&self.backstop, || {
                storage::set_reward_zone_checkpoint(&self.e, timestamp);
            });
        }

        fn mark_distribution_started(&self, timestamp: u64) {
            self.e.as_contract(&self.backstop, || {
                storage::set_reward_zone_distribution_started(&self.e);
                storage::set_reward_zone_checkpoint(&self.e, timestamp);
            });
        }

        fn mark_distribution_started_without_checkpoint(&self) {
            self.e.as_contract(&self.backstop, || {
                storage::set_reward_zone_distribution_started(&self.e);
            });
        }
    }

    #[test]
    fn membership_is_bounded_checkpoint_gated_after_distribution_and_blnd_weighted() {
        let fixture = Fixture::create();
        let client = fixture.client();
        let first = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        client.add_reward(&first, &None);
        assert_eq!(client.reward_zone(), soroban_sdk::vec![&fixture.e, first]);

        let second = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        client.add_reward(&second, &None);

        let third = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        fixture.mark_distribution_started(
            fixture.e.ledger().timestamp() - CHECKPOINT_MAX_AGE_SECONDS - 1,
        );
        assert!(client.try_add_reward(&third, &None).is_err());
        fixture.checkpoint(fixture.e.ledger().timestamp());
        client.add_reward(&third, &None);

        while client.reward_zone().len() < MAX_RZ_SIZE {
            let pool = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
            client.add_reward(&pool, &None);
        }
        let removed = client.reward_zone().last().unwrap();
        let equal = fixture.pool(2 * SCALAR_7, SCALAR_7, ACTIVATION_ENTRY_THRESHOLD_USDC);
        assert!(client
            .try_add_reward(&equal, &Some(removed.clone()))
            .is_err());

        fixture.set_pool_tier(&equal, BackstopTier::BlndUsdc, 2 * SCALAR_7, 0);
        client.add_reward(&equal, &Some(removed.clone()));
        assert_eq!(client.reward_zone().len(), MAX_RZ_SIZE);
        assert_eq!(client.reward_zone().first(), Some(equal));
        assert!(!client.reward_zone().contains(removed));
    }

    #[test]
    fn entry_requires_value_and_blnd_while_removal_uses_hysteresis() {
        let fixture = Fixture::create();
        let client = fixture.client();
        let usdc_only = fixture.pool(0, 0, ACTIVATION_ENTRY_THRESHOLD_USDC);
        assert!(client.try_add_reward(&usdc_only, &None).is_err());

        let pool = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        client.add_reward(&pool, &None);
        fixture.mark_distribution_started(fixture.e.ledger().timestamp());
        fixture.set_pool_tier(
            &pool,
            BackstopTier::Usdc,
            ACTIVATION_MAINTENANCE_THRESHOLD_USDC,
            0,
        );
        assert!(client.try_remove_reward(&pool).is_err());

        fixture.set_pool_tier(
            &pool,
            BackstopTier::Usdc,
            ACTIVATION_MAINTENANCE_THRESHOLD_USDC - 2 * SCALAR_7,
            0,
        );
        client.remove_reward(&pool);
        assert!(client.reward_zone().is_empty());
    }

    #[test]
    fn missing_checkpoint_after_distribution_fails_closed() {
        let fixture = Fixture::create();
        let client = fixture.client();
        let first = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        client.add_reward(&first, &None);
        fixture.mark_distribution_started_without_checkpoint();

        let second = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        assert!(client.try_add_reward(&second, &None).is_err());

        fixture.set_pool_tier(
            &first,
            BackstopTier::Usdc,
            ACTIVATION_MAINTENANCE_THRESHOLD_USDC - 2 * SCALAR_7,
            0,
        );
        assert!(client.try_remove_reward(&first).is_err());
    }

    #[test]
    fn ordinary_removal_needs_no_checkpoint_before_distribution_begins() {
        let fixture = Fixture::create();
        let client = fixture.client();
        let pool = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        client.add_reward(&pool, &None);
        fixture.set_pool_tier(
            &pool,
            BackstopTier::Usdc,
            ACTIVATION_MAINTENANCE_THRESHOLD_USDC - 2 * SCALAR_7,
            0,
        );

        client.remove_reward(&pool);
        assert!(client.reward_zone().is_empty());
    }

    #[test]
    fn zero_blnd_member_can_be_removed_without_a_fresh_checkpoint() {
        let fixture = Fixture::create();
        let client = fixture.client();
        let pool = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC);
        client.add_reward(&pool, &None);
        fixture.set_pool_tier(&pool, BackstopTier::BlndUsdc, SCALAR_7, SCALAR_7);
        fixture.mark_distribution_started(
            fixture.e.ledger().timestamp() - CHECKPOINT_MAX_AGE_SECONDS - 1,
        );

        client.remove_reward(&pool);
        assert!(client.reward_zone().is_empty());
    }
}
