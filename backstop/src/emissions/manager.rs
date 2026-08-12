use crate::{
    backstop::{build_pool_valuation, quote_activation, require_registered_pool, BackstopTier},
    constants::MAX_RZ_SIZE,
    dependencies::EmitterClient,
    errors::BackstopError,
    migration, storage,
};
use sep_41_token::TokenClient;
use soroban_sdk::{panic_with_error, Address, Env, Vec};

use super::distributor;
use super::tier_accounting::{
    advance_without_distribution, allocate_distribution, checked_add, checked_signed_sub,
    checkpoint_backfill, collect_weights, emissions_for_seconds, get_ongoing_emission_state,
    get_pool_ongoing_emissions, pool_weight, refresh_pool_ongoing_assets,
    set_pool_ongoing_emissions,
};

const MIN_DISTRIBUTION_INTERVAL_SECONDS: u64 = 5;
const POOL_EMISSION_GULP_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
pub(super) const CHECKPOINT_MAX_AGE_SECONDS: u64 = 60 * 60;

/// Return the pools currently eligible for BLND emissions.
pub(crate) fn get_reward_zone(e: &Env) -> Vec<Address> {
    let reward_zone = storage::get_reward_zone(e);
    if reward_zone.len() > MAX_RZ_SIZE {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    reward_zone
}

/// Add a qualifying pool to the reward zone.
pub fn add_to_reward_zone(e: &Env, to_add: Address, to_remove: Option<Address>) -> Option<Address> {
    migration::require_weight_mutation_allowed(e);
    let mut reward_zone = get_reward_zone(e);
    if reward_zone.contains(to_add.clone()) {
        panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
    }

    let valuation = build_pool_valuation(e, &to_add);
    if !quote_activation(e, &valuation.active_values, false).meets_threshold {
        panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
    }
    let entrant_weight = pool_weight(e, &to_add);
    if entrant_weight <= 0 {
        panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
    }
    if !reward_zone.is_empty() || !migration::is_active(e) {
        require_distribute_run_recently(e);
    }

    let removed = if reward_zone.len() < MAX_RZ_SIZE {
        None
    } else {
        let to_remove =
            to_remove.unwrap_or_else(|| panic_with_error!(e, BackstopError::RewardZoneFull));
        let remove_index = reward_zone
            .first_index_of(to_remove.clone())
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidRewardZoneEntry));
        let removed_weight = pool_weight(e, &to_remove);
        if entrant_weight <= removed_weight {
            panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
        }
        reward_zone.remove(remove_index);
        Some(to_remove.clone())
    };

    reward_zone.push_front(to_add.clone());
    refresh_pool_ongoing_assets(e, &to_add);
    set_reward_zone(e, &reward_zone);
    removed
}

/// Remove a pool that no longer qualifies from the reward zone.
pub fn remove_from_reward_zone(e: &Env, to_remove: Address) {
    migration::require_weight_mutation_allowed(e);
    let mut reward_zone = get_reward_zone(e);
    let remove_index = reward_zone
        .first_index_of(to_remove.clone())
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidRewardZoneEntry));
    let removed_weight = pool_weight(e, &to_remove);
    if removed_weight > 0 {
        let valuation = build_pool_valuation(e, &to_remove);
        if quote_activation(e, &valuation.active_values, true).meets_threshold {
            panic_with_error!(e, BackstopError::InvalidRewardZoneEntry);
        }
        require_distribute_run_recently(e);
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

fn require_distribute_run_recently(e: &Env) {
    if !storage::get_reward_zone_distribution_started(e) {
        return;
    }
    let now = e.ledger().timestamp();
    let checkpoint = storage::get_reward_zone_checkpoint(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::DistributionCheckpointRequired));
    if checkpoint > now
        || now
            .checked_sub(checkpoint)
            .is_none_or(|age| age > CHECKPOINT_MAX_AGE_SECONDS)
    {
        panic_with_error!(e, BackstopError::DistributionCheckpointRequired);
    }
}

pub fn distribute(e: &Env) -> i128 {
    if !migration::is_active(e) {
        return match migration::distribution_transition(e) {
            migration::DistributionTransition::Backfill(checkpoint) => {
                checkpoint_backfill(e, checkpoint)
            }
            migration::DistributionTransition::Activated(checkpoint) => {
                let mut state = get_ongoing_emission_state(e);
                advance_without_distribution(e, &mut state, checkpoint)
            }
        };
    }

    let backstop = e.current_contract_address();
    let emitter = EmitterClient::new(e, &storage::get_emitter(e));
    if emitter.get_backstop() != backstop {
        panic_with_error!(e, BackstopError::EmitterDidNotMigrate);
    }
    if storage::get_reward_zone(e).is_empty() {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }

    let mut state = get_ongoing_emission_state(e);
    let (weights, total_eligible_blnd) = collect_weights(e, true);
    let last_distribution = state.last_distribution.unwrap();
    let blnd = TokenClient::new(e, &storage::get_blnd_token(e));
    let binding_verified = storage::get_blnd_binding_verified(e);
    let emitter_checkpoint_before = emitter.get_last_distro(&backstop);
    let balance_before = if binding_verified {
        None
    } else {
        Some(blnd.balance(&backstop))
    };
    let emitted = emitter.distribute();
    if emitted < 0 {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let checkpoint = emitter.get_last_distro(&backstop);
    let elapsed = checkpoint
        .checked_sub(last_distribution)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::DistributionTooSoon));
    let current_elapsed = checkpoint
        .checked_sub(emitter_checkpoint_before)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidOngoingBalance));
    if elapsed < MIN_DISTRIBUTION_INTERVAL_SECONDS {
        panic_with_error!(e, BackstopError::DistributionTooSoon);
    }
    if emitter_checkpoint_before < last_distribution
        || checkpoint > e.ledger().timestamp()
        || emitted != emissions_for_seconds(e, current_elapsed)
    {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }

    if let Some(balance_before) = balance_before {
        let binding_delta = checked_signed_sub(e, blnd.balance(&backstop), balance_before);
        if emitted <= 0 || binding_delta != emitted {
            panic_with_error!(e, BackstopError::InvalidOngoingBalance);
        }
    }

    let result = allocate_distribution(
        e,
        &mut state,
        emissions_for_seconds(e, elapsed),
        checkpoint,
        weights,
        total_eligible_blnd,
    );
    if !binding_verified {
        storage::set_blnd_binding_verified(e);
    }
    result
}

pub fn gulp_emissions(e: &Env, pool: &Address) -> (i128, i128) {
    pool.require_auth();
    require_registered_pool(e, pool);

    let now = e.ledger().timestamp();
    if storage::get_pool_emission_gulp(e, pool).is_some_and(|last_gulp| {
        last_gulp
            .checked_add(POOL_EMISSION_GULP_INTERVAL_SECONDS)
            .is_none_or(|next_gulp| next_gulp > now)
    }) {
        panic_with_error!(e, BackstopError::PoolEmissionGulpTooSoon);
    }

    let mut pool_state = get_pool_ongoing_emissions(e, pool);
    let backstop_amount = checked_add(e, pool_state.pending_blnd_usdc, pool_state.pending_blnd_xlm);
    let pool_amount = pool_state.accrued_pool;
    if backstop_amount == 0 && pool_amount == 0 {
        return (0, 0);
    }
    set_backstop_emission_eps(
        e,
        BackstopTier::BlndUsdc,
        pool,
        pool_state.pending_blnd_usdc,
    );
    set_backstop_emission_eps(e, BackstopTier::BlndXlm, pool, pool_state.pending_blnd_xlm);
    pool_state.pending_blnd_usdc = 0;
    pool_state.pending_blnd_xlm = 0;
    pool_state.accrued_pool = 0;

    if pool_amount > 0 {
        let backstop = e.current_contract_address();
        let blnd = TokenClient::new(e, &storage::get_blnd_token(e));
        let allowance = checked_add(e, blnd.allowance(&backstop, pool), pool_amount);
        let expiration_ledger = e
            .ledger()
            .sequence()
            .checked_add(storage::LEDGER_BUMP_USER)
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        blnd.approve(&backstop, pool, &allowance, &expiration_ledger);
    }

    set_pool_ongoing_emissions(e, pool, &pool_state);
    storage::set_pool_emission_gulp(e, pool, now);
    (backstop_amount, pool_amount)
}

/// Set a fresh seven-day emission stream for one backstop tier.
pub fn set_backstop_emission_eps(e: &Env, tier: BackstopTier, pool: &Address, pending: i128) {
    distributor::set_backstop_emission_eps(e, tier, pool, pending);
}

#[cfg(test)]
mod tests {
    use mock_pool_factory::MockPoolFactoryClient;
    use sep_41_token::{testutils::MockTokenClient, TokenClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        vec, Address, Env,
    };

    use crate::{
        backstop::{BackstopTier, PoolBalance, UserBalance},
        constants::{MAX_RZ_SIZE, SCALAR_7},
        emissions::preview_user_ongoing_blnd,
        migration,
        storage::{self, OngoingEmissionState, PoolOngoingEmissions},
        testutils::{
            create_backstop, create_blnd_token, create_comet_lp_pool, create_emitter,
            create_mock_pool, create_mock_pool_factory, create_token, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    struct Fixture {
        admin: Address,
        backstop: Address,
        blnd: Address,
        blnd_usdc: Address,
        blnd_xlm: Address,
        e: Env,
        factory: Address,
    }

    impl Fixture {
        fn create() -> Self {
            let e = Env::default();
            e.mock_all_auths_allowing_non_root_auth();
            e.cost_estimate().budget().reset_unlimited();
            e.ledger().set_timestamp(1_000);

            let backstop = create_backstop(&e);
            let admin = Address::generate(&e);
            let (blnd, blnd_client) = create_blnd_token(&e, &backstop, &admin);
            let (usdc, _) = create_usdc_token(&e, &backstop, &admin);
            let (xlm, _) = create_token(&e, &admin);
            let (blnd_usdc, _) = create_comet_lp_pool(&e, &admin, &blnd, &usdc);
            let (blnd_xlm, _) = create_comet_lp_pool(&e, &admin, &blnd, &xlm);
            let (factory, _) = create_mock_pool_factory(&e, &backstop);
            e.as_contract(&backstop, || {
                storage::set_blnd_usdc_token(&e, &blnd_usdc);
                storage::set_blnd_xlm_token(&e, &blnd_xlm);
            });
            let (emitter, _) = create_emitter(&e, &backstop, &blnd_usdc, &blnd, 1_000);
            blnd_client.set_admin(&emitter);
            e.as_contract(&backstop, || {
                migration::activate_for_test(&e, 1_000);
            });

            Self {
                admin,
                backstop,
                blnd,
                blnd_usdc,
                blnd_xlm,
                e,
                factory,
            }
        }

        fn client(&self) -> BackstopClient<'_> {
            BackstopClient::new(&self.e, &self.backstop)
        }

        fn claimable(&self, tier: &BackstopTier, user: &Address, pools: &Vec<Address>) -> i128 {
            self.e.as_contract(&self.backstop, || {
                preview_user_ongoing_blnd(&self.e, *tier, user, pools)
            })
        }

        fn pool_emissions(&self, pool: &Address) -> PoolOngoingEmissions {
            self.e
                .as_contract(&self.backstop, || get_pool_ongoing_emissions(&self.e, pool))
        }

        fn ongoing_state(&self) -> OngoingEmissionState {
            self.e
                .as_contract(&self.backstop, || get_ongoing_emission_state(&self.e))
        }

        fn blnd_binding_verified(&self) -> bool {
            self.e.as_contract(&self.backstop, || {
                storage::get_blnd_binding_verified(&self.e)
            })
        }

        fn distribution(&self) -> i128 {
            self.e
                .as_contract(&self.backstop, || super::distribute(&self.e))
        }

        fn pool(&self, blnd_usdc: i128, blnd_xlm: i128) -> Address {
            let (pool, _) = create_mock_pool(&self.e, &self.backstop);
            MockPoolFactoryClient::new(&self.e, &self.factory).set_pool(&pool);
            self.e.as_contract(&self.backstop, || {
                storage::set_pool_balance_for_tier(
                    &self.e,
                    BackstopTier::BlndUsdc,
                    &pool,
                    &PoolBalance {
                        q4w: 0,
                        shares: blnd_usdc,
                        tokens: blnd_usdc,
                    },
                );
                storage::set_pool_balance_for_tier(
                    &self.e,
                    BackstopTier::BlndXlm,
                    &pool,
                    &PoolBalance {
                        q4w: 0,
                        shares: blnd_xlm,
                        tokens: blnd_xlm,
                    },
                );
            });
            pool
        }

        fn set_reward_zone(&self, pools: &Vec<Address>) {
            self.e.as_contract(&self.backstop, || {
                storage::set_reward_zone(&self.e, pools);
                for pool in pools.iter() {
                    refresh_pool_ongoing_assets(&self.e, &pool);
                }
            });
        }

        fn user_position(&self, tier: BackstopTier, user: &Address, pool: &Address, shares: i128) {
            self.e.as_contract(&self.backstop, || {
                storage::set_user_balance_for_tier(
                    &self.e,
                    tier,
                    pool,
                    user,
                    &UserBalance {
                        shares,
                        q4w: vec![&self.e],
                    },
                );
            });
        }
    }

    #[test]
    fn backfill_weights_use_active_blnd_usdc_lp_tokens() {
        let fixture = Fixture::create();
        let first = fixture.pool(10 * SCALAR_7, 0);
        let second = fixture.pool(20 * SCALAR_7, 20 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, first.clone(), second.clone()]);

        let (weights, total_weight) = fixture
            .e
            .as_contract(&fixture.backstop, || collect_weights(&fixture.e, false));

        assert_eq!(total_weight, 30 * SCALAR_7);
        assert_eq!(
            weights,
            vec![
                &fixture.e,
                (
                    first,
                    PoolOngoingEmissions {
                        accrued_pool: 0,
                        active_blnd_usdc: 10 * SCALAR_7,
                        active_blnd_xlm: 0,
                        backstop_tier_carry: 0,
                        pending_blnd_usdc: 0,
                        pending_blnd_xlm: 0,
                    },
                    10 * SCALAR_7,
                    0,
                ),
                (
                    second,
                    PoolOngoingEmissions {
                        accrued_pool: 0,
                        active_blnd_usdc: 20 * SCALAR_7,
                        active_blnd_xlm: 20 * SCALAR_7,
                        backstop_tier_carry: 0,
                        pending_blnd_usdc: 0,
                        pending_blnd_xlm: 0,
                    },
                    20 * SCALAR_7,
                    0,
                ),
            ]
        );
    }

    #[test]
    fn tier_stream_rolls_unfinished_emissions_into_a_fresh_seven_days() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        let active_shares = 10 * SCALAR_7;
        let allocation = 7 * SCALAR_7;
        let start = 1_000;
        fixture.user_position(BackstopTier::BlndUsdc, &user, &pool, active_shares);
        fixture.e.as_contract(&fixture.backstop, || {
            set_backstop_emission_eps(&fixture.e, BackstopTier::BlndUsdc, &pool, allocation);
        });
        let stream = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_backstop_emis_data(&fixture.e, BackstopTier::BlndUsdc, &pool).unwrap()
        });
        assert_eq!(stream.expiration, start + distributor::STREAM_SECONDS);

        let next_gulp = start + 24 * 60 * 60;
        fixture.e.ledger().set_timestamp(next_gulp);
        let first_day = fixture.e.as_contract(&fixture.backstop, || {
            distributor::update_emissions(&fixture.e, BackstopTier::BlndUsdc, &pool, &user)
        });
        assert!((SCALAR_7 - 1..=SCALAR_7).contains(&first_day.accrued));

        fixture.e.as_contract(&fixture.backstop, || {
            set_backstop_emission_eps(&fixture.e, BackstopTier::BlndUsdc, &pool, allocation);
        });
        fixture
            .e
            .ledger()
            .set_timestamp(next_gulp + distributor::STREAM_SECONDS);
        let completed = fixture.e.as_contract(&fixture.backstop, || {
            distributor::update_emissions(&fixture.e, BackstopTier::BlndUsdc, &pool, &user)
        });
        assert_eq!(completed.accrued, 2 * allocation);
        let stream = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_backstop_emis_data(&fixture.e, BackstopTier::BlndUsdc, &pool).unwrap()
        });
        assert_eq!(stream.schedule_carry, 0);
    }

    #[test]
    fn allocates_by_emitter_checkpoint_and_ignores_unrelated_blnd() {
        let fixture = Fixture::create();
        let first = fixture.pool(10 * SCALAR_7, 0);
        let second = fixture.pool(0, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, first.clone(), second.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        assert_eq!(fixture.distribution(), 10 * SCALAR_7);
        for pool in [first.clone(), second.clone()] {
            assert_eq!(
                fixture.pool_emissions(&pool),
                PoolOngoingEmissions {
                    accrued_pool: 15_000_000,
                    active_blnd_usdc: if pool == first { 10 * SCALAR_7 } else { 0 },
                    active_blnd_xlm: if pool == second { 10 * SCALAR_7 } else { 0 },
                    backstop_tier_carry: 0,
                    pending_blnd_usdc: if pool == first { 35_000_000 } else { 0 },
                    pending_blnd_xlm: if pool == second { 35_000_000 } else { 0 },
                }
            );
        }
        assert!(fixture.blnd_binding_verified());

        MockTokenClient::new(&fixture.e, &fixture.blnd).mint(&fixture.backstop, &1);
        fixture.e.ledger().set_timestamp(1_015);
        assert_eq!(fixture.distribution(), 5 * SCALAR_7);
        assert_eq!(
            fixture.ongoing_state(),
            OngoingEmissionState {
                backstop_allocated: 105_000_000,
                backstop_carry: 0,
                backstop_claimed: 0,
                last_distribution: Some(1_015),
                pool_allocated: 45_000_000,
                pool_carry: 0,
                split_carry: 0,
                total_distributed: 15 * SCALAR_7,
            }
        );
    }

    #[test]
    fn first_positive_distribution_binds_the_configured_blnd_token() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool]);
        fixture.e.ledger().set_timestamp(1_005);

        assert!(!fixture.blnd_binding_verified());
        assert_eq!(fixture.client().distribute(), 5 * SCALAR_7);
        assert!(fixture.blnd_binding_verified());
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            5 * SCALAR_7
        );
    }

    #[test]
    fn direct_emitter_call_is_allocated_once_at_the_next_checkpoint() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool]);
        let emitter = fixture
            .e
            .as_contract(&fixture.backstop, || storage::get_emitter(&fixture.e));
        let emitter = EmitterClient::new(&fixture.e, &emitter);

        fixture.e.ledger().set_timestamp(1_005);
        assert_eq!(emitter.distribute(), 5 * SCALAR_7);
        fixture.e.ledger().set_timestamp(1_010);
        assert_eq!(fixture.client().distribute(), 10 * SCALAR_7);
        assert!(fixture.blnd_binding_verified());

        fixture.e.ledger().set_timestamp(1_015);
        assert_eq!(fixture.client().distribute(), 5 * SCALAR_7);
        assert_eq!(fixture.ongoing_state().total_distributed, 15 * SCALAR_7);
    }

    #[test]
    fn claim_preview_is_read_only() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        fixture.user_position(BackstopTier::BlndUsdc, &user, &pool, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);
        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone()],
            ),
            0
        );
        assert_eq!(fixture.client().gulp_emissions(&pool), 3 * SCALAR_7);
        fixture
            .e
            .ledger()
            .set_timestamp(1_010 + distributor::STREAM_SECONDS);

        let stored_before = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_user_emis_data(&fixture.e, BackstopTier::BlndUsdc, &pool, &user)
        });
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone()],
            ),
            7 * SCALAR_7
        );
        assert!(fixture.e.auths().is_empty());
        let stored_after = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_user_emis_data(&fixture.e, BackstopTier::BlndUsdc, &pool, &user)
        });
        assert_eq!(stored_after, stored_before);
    }

    #[test]
    fn users_claim_only_the_two_blnd_tier_allocations() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 10 * SCALAR_7);
        let blnd_usdc_user = Address::generate(&fixture.e);
        let blnd_xlm_user = Address::generate(&fixture.e);
        fixture.user_position(
            BackstopTier::BlndUsdc,
            &blnd_usdc_user,
            &pool,
            10 * SCALAR_7,
        );
        fixture.user_position(BackstopTier::BlndXlm, &blnd_xlm_user, &pool, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(fixture.client().gulp_emissions(&pool), 3 * SCALAR_7);
        fixture
            .e
            .ledger()
            .set_timestamp(1_010 + distributor::STREAM_SECONDS);
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndUsdc,
                &blnd_usdc_user,
                &vec![&fixture.e, pool.clone()],
            ),
            35_000_000
        );
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndXlm,
                &blnd_xlm_user,
                &vec![&fixture.e, pool.clone()],
            ),
            35_000_000
        );

        // Refresh the reward-zone checkpoint before claims compound into the
        // two tier positions.
        fixture.client().distribute();

        let blnd_before = TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop);
        let blnd_usdc_before =
            TokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.backstop);
        let blnd_xlm_before =
            TokenClient::new(&fixture.e, &fixture.blnd_xlm).balance(&fixture.backstop);
        let blnd_usdc_out = fixture.client().claim(
            &BackstopTier::BlndUsdc,
            &blnd_usdc_user,
            &vec![&fixture.e, pool.clone()],
            &0,
        );
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndUsdc,
                &blnd_usdc_user,
                &vec![&fixture.e, pool.clone()],
            ),
            0
        );
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndXlm,
                &blnd_xlm_user,
                &vec![&fixture.e, pool.clone()],
            ),
            35_000_000
        );
        let blnd_xlm_out = fixture.client().claim(
            &BackstopTier::BlndXlm,
            &blnd_xlm_user,
            &vec![&fixture.e, pool.clone()],
            &0,
        );
        assert!(blnd_usdc_out > 0);
        assert!(blnd_xlm_out > 0);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            blnd_before - 70_000_000
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.backstop),
            blnd_usdc_before + blnd_usdc_out
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd_xlm).balance(&fixture.backstop),
            blnd_xlm_before + blnd_xlm_out
        );
        assert_eq!(fixture.ongoing_state().backstop_claimed, 7 * SCALAR_7);
        assert!(fixture.pool_emissions(&pool).accrued_pool > 3 * SCALAR_7);
    }

    #[test]
    fn failed_compounding_preserves_the_tier_accrual() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        fixture.user_position(BackstopTier::BlndUsdc, &user, &pool, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(fixture.client().gulp_emissions(&pool), 3 * SCALAR_7);
        fixture
            .e
            .ledger()
            .set_timestamp(1_010 + distributor::STREAM_SECONDS);
        fixture.client().distribute();
        let accrued = fixture.claimable(
            &BackstopTier::BlndUsdc,
            &user,
            &vec![&fixture.e, pool.clone()],
        );
        let blnd_before = TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop);
        let shares_before = fixture
            .client()
            .user_balance(&BackstopTier::BlndUsdc, &pool, &user)
            .shares;

        assert!(fixture
            .client()
            .try_claim(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone()],
                &i128::MAX,
            )
            .is_err());
        assert_eq!(
            fixture.claimable(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone()],
            ),
            accrued
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            blnd_before
        );
        assert_eq!(
            fixture
                .client()
                .user_balance(&BackstopTier::BlndUsdc, &pool, &user)
                .shares,
            shares_before
        );
        assert_eq!(fixture.ongoing_state().backstop_claimed, 0);
    }

    #[test]
    fn claim_batches_pool_addresses_for_one_tier() {
        let fixture = Fixture::create();
        let first = fixture.pool(10 * SCALAR_7, 0);
        let second = fixture.pool(20 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        fixture.user_position(BackstopTier::BlndUsdc, &user, &first, 10 * SCALAR_7);
        fixture.user_position(BackstopTier::BlndUsdc, &user, &second, 20 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, first.clone(), second.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert!(fixture.client().gulp_emissions(&first) > 0);
        assert!(fixture.client().gulp_emissions(&second) > 0);
        fixture
            .e
            .ledger()
            .set_timestamp(1_010 + distributor::STREAM_SECONDS);
        fixture.client().distribute();

        let pool_addresses = vec![&fixture.e, first.clone(), second.clone()];
        let aggregate_claim = fixture.claimable(&BackstopTier::BlndUsdc, &user, &pool_addresses);
        assert!(aggregate_claim > 0);
        let first_shares = fixture
            .client()
            .user_balance(&BackstopTier::BlndUsdc, &first, &user)
            .shares;
        let second_shares = fixture
            .client()
            .user_balance(&BackstopTier::BlndUsdc, &second, &user)
            .shares;
        let blnd_before = TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop);
        let lp_before = TokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.backstop);

        let lp_out = fixture
            .client()
            .claim(&BackstopTier::BlndUsdc, &user, &pool_addresses, &0);

        assert!(lp_out > 0);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            blnd_before - aggregate_claim
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.backstop),
            lp_before + lp_out
        );
        assert_eq!(
            fixture.claimable(&BackstopTier::BlndUsdc, &user, &pool_addresses,),
            0
        );
        assert!(
            fixture
                .client()
                .user_balance(&BackstopTier::BlndUsdc, &first, &user)
                .shares
                > first_shares
        );
        assert!(
            fixture
                .client()
                .user_balance(&BackstopTier::BlndUsdc, &second, &user)
                .shares
                > second_shares
        );
        assert_eq!(fixture.ongoing_state().backstop_claimed, aggregate_claim);
    }

    #[test]
    fn claim_rejects_invalid_scope() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);

        assert!(fixture
            .client()
            .try_claim(&BackstopTier::BlndUsdc, &user, &vec![&fixture.e], &0,)
            .is_err());
        assert!(fixture
            .client()
            .try_claim(
                &BackstopTier::Usdc,
                &user,
                &vec![&fixture.e, pool.clone()],
                &0,
            )
            .is_err());
        assert!(fixture
            .client()
            .try_claim(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, Address::generate(&fixture.e)],
                &0,
            )
            .is_err());
        assert!(fixture
            .client()
            .try_claim(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone(), pool],
                &0,
            )
            .is_err());
    }

    #[test]
    fn pool_tranche_is_allowed_and_spent_by_the_registered_pool() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let recipient = Address::generate(&fixture.e);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(fixture.client().gulp_emissions(&pool), 3 * SCALAR_7);
        let blnd = TokenClient::new(&fixture.e, &fixture.blnd);
        assert_eq!(blnd.allowance(&fixture.backstop, &pool), 3 * SCALAR_7);
        assert_eq!(
            fixture.e.as_contract(&fixture.backstop, || {
                storage::get_pool_emission_gulp(&fixture.e, &pool)
            }),
            Some(1_010)
        );
        assert!(fixture.client().try_gulp_emissions(&pool).is_err());

        fixture.e.as_contract(&pool, || {
            blnd.transfer_from(&pool, &fixture.backstop, &recipient, &SCALAR_7);
        });
        assert_eq!(blnd.allowance(&fixture.backstop, &pool), 2 * SCALAR_7);
        assert_eq!(blnd.balance(&recipient), SCALAR_7);

        fixture.e.ledger().set_timestamp(1_015);
        assert_eq!(fixture.client().distribute(), 5 * SCALAR_7);
    }

    #[test]
    fn rejects_emitter_output_in_a_different_token() {
        let fixture = Fixture::create();
        let (other_blnd, other_blnd_client) =
            create_token(&fixture.e, &Address::generate(&fixture.e));
        let (emitter, _) = create_emitter(
            &fixture.e,
            &fixture.backstop,
            &fixture.blnd_usdc,
            &other_blnd,
            1_000,
        );
        other_blnd_client.set_admin(&emitter);
        let pool = fixture.pool(10 * SCALAR_7, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool]);
        fixture.e.ledger().set_timestamp(1_005);

        assert!(fixture.client().try_distribute().is_err());
        assert!(!fixture.blnd_binding_verified());
        assert_eq!(
            fixture.ongoing_state(),
            OngoingEmissionState {
                backstop_allocated: 0,
                backstop_carry: 0,
                backstop_claimed: 0,
                last_distribution: Some(1_000),
                pool_allocated: 0,
                pool_carry: 0,
                split_carry: 0,
                total_distributed: 0,
            }
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &other_blnd).balance(&fixture.backstop),
            0
        );
    }

    #[test]
    fn maximum_reward_zone_is_distributed_in_one_call() {
        let fixture = Fixture::create();
        let mut pools = Vec::new(&fixture.e);
        for _ in 0..MAX_RZ_SIZE {
            pools.push_back(fixture.pool(SCALAR_7, 0));
        }
        fixture.set_reward_zone(&pools);
        fixture.e.ledger().set_timestamp(1_005);

        assert_eq!(fixture.distribution(), 5 * SCALAR_7);
        for pool in pools.iter() {
            let emissions = fixture.pool_emissions(&pool);
            assert!(emissions.pending_blnd_usdc > 0 || emissions.pending_blnd_xlm > 0);
            assert!(emissions.accrued_pool > 0);
        }
    }

    #[test]
    fn zero_eligible_blnd_is_carried_until_a_blnd_tier_is_deposited() {
        let fixture = Fixture::create();
        let pool = fixture.pool(0, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);
        fixture.e.ledger().set_timestamp(1_005);

        assert_eq!(fixture.distribution(), 5 * SCALAR_7);
        assert_eq!(
            fixture.ongoing_state(),
            OngoingEmissionState {
                backstop_allocated: 0,
                backstop_carry: 35_000_000,
                backstop_claimed: 0,
                last_distribution: Some(1_005),
                pool_allocated: 0,
                pool_carry: 15_000_000,
                split_carry: 0,
                total_distributed: 5 * SCALAR_7,
            }
        );

        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.admin,
            &pool,
            &SCALAR_7,
        );
        assert_eq!(fixture.pool_emissions(&pool).active_blnd_usdc, SCALAR_7);

        fixture.e.ledger().set_timestamp(1_010);
        assert_eq!(fixture.distribution(), 5 * SCALAR_7);
    }

    #[test]
    fn stale_checkpoint_rejects_active_weight_changes() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_user_balance_for_tier(
                &fixture.e,
                BackstopTier::BlndUsdc,
                &pool,
                &user,
                &UserBalance {
                    shares: 10 * SCALAR_7,
                    q4w: vec![&fixture.e],
                },
            );
        });
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);
        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();

        fixture.e.ledger().set_timestamp(1_016);
        assert!(fixture
            .client()
            .try_queue_withdrawal(&crate::BackstopTier::BlndUsdc, &user, &pool, &SCALAR_7)
            .is_err());
    }

    #[test]
    fn fresh_checkpoint_allows_and_refreshes_active_weight_changes() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_user_balance_for_tier(
                &fixture.e,
                BackstopTier::BlndUsdc,
                &pool,
                &user,
                &UserBalance {
                    shares: 10 * SCALAR_7,
                    q4w: vec![&fixture.e],
                },
            );
        });
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);
        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();

        fixture.e.ledger().set_timestamp(1_016);
        fixture.client().distribute();
        fixture
            .client()
            .queue_withdrawal(&crate::BackstopTier::BlndUsdc, &user, &pool, &SCALAR_7);
        assert_eq!(fixture.pool_emissions(&pool).active_blnd_usdc, 9 * SCALAR_7);
    }
}

#[cfg(test)]
mod reward_zone_tests {
    use mock_pool_factory::MockPoolFactoryClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env,
    };

    use crate::{
        backstop::{BackstopTier, PoolBalance},
        constants::{
            ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC, MAX_RZ_SIZE,
            SCALAR_7,
        },
        storage,
        testutils::{
            create_backstop, create_blnd_token, create_comet_lp_pool, create_mock_pool_factory,
            create_token, create_usdc_token,
        },
        BackstopClient,
    };

    use super::CHECKPOINT_MAX_AGE_SECONDS;

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
            MockPoolFactoryClient::new(&self.e, &self.factory).set_pool(&pool);
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
    fn first_pre_activation_member_cannot_receive_prior_distribution_time() {
        let fixture = Fixture::create();
        let client = fixture.client();
        let first = fixture.pool(SCALAR_7, 0, ACTIVATION_ENTRY_THRESHOLD_USDC - SCALAR_7);
        fixture.mark_distribution_started(
            fixture.e.ledger().timestamp() - CHECKPOINT_MAX_AGE_SECONDS - 1,
        );

        assert!(client.try_add_reward(&first, &None).is_err());
        fixture.checkpoint(fixture.e.ledger().timestamp());
        client.add_reward(&first, &None);
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
