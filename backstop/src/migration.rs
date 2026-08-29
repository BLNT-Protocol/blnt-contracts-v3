use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Env};

use crate::{
    constants::MAX_BACKFILLED_EMISSIONS,
    dependencies::{EmitterClient, Swap},
    errors::BackstopError,
    events::BackstopEvents,
    storage::{self, OngoingEmissionState},
};

const DAY_IN_SECONDS: u64 = 24 * 60 * 60;
const QUEUE_ATTESTATION_WINDOW_SECONDS: u64 = 7 * DAY_IN_SECONDS;
const ACTIVATION_GRACE_SECONDS: u64 = 7 * DAY_IN_SECONDS;

#[derive(Clone)]
#[contracttype]
enum MigrationDataKey {
    MigrationEpochStart,
    VerifiedQueueUnlock,
    ActivatedAt,
    BackfillEnd,
    ScheduledBackfill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
#[cfg(test)]
pub(crate) enum MigrationStatus {
    Pending,
    Open,
    Active,
}

/// Complete observable state for the incumbent-emitter migration lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
#[cfg(test)]
pub(crate) struct MigrationState {
    pub activated_at: Option<u64>,
    pub backfill_end: Option<u64>,
    pub blnt_binding_verified: bool,
    pub funded_backfill: Option<i128>,
    pub migration_epoch_start: Option<u64>,
    pub scheduled_backfill: i128,
    pub status: MigrationStatus,
    pub verified_queue_unlock: Option<u64>,
}

pub(crate) enum DistributionTransition {
    Backfill(u64),
    Activated(u64),
}

pub(crate) fn is_active(e: &Env) -> bool {
    activated_at(e).is_some()
}

pub(crate) fn require_active(e: &Env) {
    if !is_active(e) {
        panic_with_error!(e, BackstopError::MigrationNotActive);
    }
}

/// Fail closed after the emitter replacement until `distribute` transitions
/// the local accounting from backfill to ongoing emissions.
pub(crate) fn require_weight_mutation_allowed(e: &Env) {
    if is_active(e) {
        return;
    }
    if emitter(e).get_backstop() == e.current_contract_address() {
        panic_with_error!(e, BackstopError::MigrationNotActive);
    }
}

pub(crate) fn require_backfill_funded(e: &Env) {
    require_active(e);
    let scheduled = scheduled_backfill(e);
    if scheduled > 0 && storage::get_backfill_funded_amount(e) != Some(scheduled) {
        panic_with_error!(e, BackstopError::BackfillNotFunded);
    }
}

#[cfg(test)]
pub(crate) fn status(e: &Env) -> MigrationStatus {
    if is_active(e) {
        MigrationStatus::Active
    } else if migration_epoch_start(e).is_some() {
        MigrationStatus::Open
    } else {
        MigrationStatus::Pending
    }
}

#[cfg(test)]
pub(crate) fn state(e: &Env) -> MigrationState {
    MigrationState {
        activated_at: activated_at(e),
        backfill_end: backfill_end(e),
        blnt_binding_verified: storage::get_blnt_binding_verified(e),
        funded_backfill: funded_backfill(e),
        migration_epoch_start: migration_epoch_start(e),
        scheduled_backfill: scheduled_backfill(e),
        status: status(e),
        verified_queue_unlock: verified_queue_unlock(e),
    }
}

pub(crate) fn migration_epoch_start(e: &Env) -> Option<u64> {
    e.storage()
        .instance()
        .get(&MigrationDataKey::MigrationEpochStart)
}

pub(crate) fn verified_queue_unlock(e: &Env) -> Option<u64> {
    e.storage()
        .instance()
        .get(&MigrationDataKey::VerifiedQueueUnlock)
}

pub(crate) fn activated_at(e: &Env) -> Option<u64> {
    e.storage().instance().get(&MigrationDataKey::ActivatedAt)
}

#[cfg(test)]
pub(crate) fn backfill_end(e: &Env) -> Option<u64> {
    e.storage().instance().get(&MigrationDataKey::BackfillEnd)
}

pub(crate) fn scheduled_backfill(e: &Env) -> i128 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::ScheduledBackfill)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn funded_backfill(e: &Env) -> Option<i128> {
    storage::get_backfill_funded_amount(e)
}

pub(crate) fn record_backfill_distribution(e: &Env, amount: i128) {
    if amount < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let next = scheduled_backfill(e)
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if next > MAX_BACKFILLED_EMISSIONS {
        panic_with_error!(e, BackstopError::MaxBackfillEmissions);
    }
    e.storage()
        .instance()
        .set(&MigrationDataKey::ScheduledBackfill, &next);
}

pub(crate) fn distribution_transition(e: &Env) -> DistributionTransition {
    require_not_active(e);
    let client = emitter(e);
    let candidate = e.current_contract_address();
    let now = e.ledger().timestamp();

    if client.get_backstop() == candidate {
        return DistributionTransition::Activated(activate(e));
    }

    if let Some(queued) = client.get_queued_swap() {
        require_valid_queue(e, &queued);
        attest_queue(e, &queued, now);
    } else {
        e.storage()
            .instance()
            .remove(&MigrationDataKey::VerifiedQueueUnlock);
    }

    if migration_epoch_start(e).is_none() {
        open_backfill_epoch(e, now);
    }
    DistributionTransition::Backfill(now)
}

pub(crate) fn drop(e: &Env) {
    if storage::get_backfill_funded_amount(e).is_some() {
        panic_with_error!(e, BackstopError::BackfillAlreadyFunded);
    }
    require_active(e);
    let scheduled = scheduled_backfill(e);
    if !(0..=MAX_BACKFILLED_EMISSIONS).contains(&scheduled) {
        panic_with_error!(e, BackstopError::InvalidBackfillFunding);
    }

    // Persist before the external call so reentrancy or repetition cannot
    // create two obligations. A later failure rolls this marker back.
    storage::set_backfill_funded_amount(e, scheduled);
    let candidate = e.current_contract_address();
    let blnt = TokenClient::new(e, &storage::get_blnt_token(e));
    let balance_before = blnt.balance(&candidate);
    let mut recipients = storage::get_drop_list(e);
    let mut expected_candidate_delta = scheduled;
    for (recipient, amount) in recipients.iter() {
        if recipient == candidate {
            expected_candidate_delta = expected_candidate_delta
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        }
    }
    if scheduled > 0 {
        recipients.push_back((candidate.clone(), scheduled));
    }
    emitter(e).drop(&recipients);
    let received = checked_signed_sub(e, blnt.balance(&candidate), balance_before);
    if received != expected_candidate_delta {
        panic_with_error!(e, BackstopError::InvalidBackfillFunding);
    }
    storage::set_blnt_binding_verified(e);
    BackstopEvents::backfill_funded(e, scheduled);
}

fn activate(e: &Env) -> u64 {
    let activated = e.ledger().timestamp();
    if let Some(verified_unlock) = verified_queue_unlock(e) {
        let activation_deadline = verified_unlock
            .checked_add(ACTIVATION_GRACE_SECONDS)
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        if activated < verified_unlock || activated > activation_deadline {
            panic_with_error!(e, BackstopError::SyncWindowExpired);
        }
    } else if migration_epoch_start(e).is_some() {
        panic_with_error!(e, BackstopError::MigrationNotPrepared);
    }
    let last_distribution = emitter(e).get_last_distro(&e.current_contract_address());
    if last_distribution > activated {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let mut state = storage::get_ongoing_emission_state(e);
    let backfill_end = state.last_distribution.unwrap_or(last_distribution);
    if backfill_end > last_distribution {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    state.last_distribution = Some(last_distribution);
    storage::set_ongoing_emission_state(e, &state);
    e.storage()
        .instance()
        .set(&MigrationDataKey::ActivatedAt, &activated);
    e.storage()
        .instance()
        .set(&MigrationDataKey::BackfillEnd, &backfill_end);
    BackstopEvents::migration_activated(e, activated, backfill_end);
    last_distribution
}

fn emitter(e: &Env) -> EmitterClient<'_> {
    EmitterClient::new(e, &storage::get_emitter(e))
}

fn require_not_active(e: &Env) {
    if is_active(e) {
        panic_with_error!(e, BackstopError::AlreadyFinalized);
    }
}

fn require_valid_queue(e: &Env, queued: &Swap) {
    if queued.new_backstop != e.current_contract_address()
        || queued.new_backstop_token != storage::get_blnt_xlm_token(e)
    {
        panic_with_error!(e, BackstopError::InvalidQueuedSwap);
    }
}

fn attest_queue(e: &Env, queued: &Swap, now: u64) {
    let attestation_start = queued
        .unlock_time
        .saturating_sub(QUEUE_ATTESTATION_WINDOW_SECONDS);
    let attestation_end = queued
        .unlock_time
        .checked_add(ACTIVATION_GRACE_SECONDS)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if now >= attestation_start && now <= attestation_end {
        e.storage()
            .instance()
            .set(&MigrationDataKey::VerifiedQueueUnlock, &queued.unlock_time);
    } else {
        e.storage()
            .instance()
            .remove(&MigrationDataKey::VerifiedQueueUnlock);
    }
}

fn open_backfill_epoch(e: &Env, epoch_start: u64) {
    e.storage()
        .instance()
        .set(&MigrationDataKey::MigrationEpochStart, &epoch_start);
    storage::set_ongoing_emission_state(
        e,
        &OngoingEmissionState {
            backstop_allocated: 0,
            backstop_carry: 0,
            backstop_claimed: 0,
            last_distribution: Some(epoch_start),
            pool_allocated: 0,
            pool_carry: 0,
            split_carry: 0,
            total_distributed: 0,
        },
    );
    storage::set_reward_zone_checkpoint(e, epoch_start);
    storage::set_reward_zone_distribution_started(e);
}

fn checked_signed_sub(e: &Env, lhs: i128, rhs: i128) -> i128 {
    lhs.checked_sub(rhs)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(any(test, feature = "testutils"))]
pub fn activate_for_test(e: &Env, checkpoint: u64) {
    e.storage()
        .instance()
        .set(&MigrationDataKey::ActivatedAt, &checkpoint);
    storage::set_ongoing_emission_state(
        e,
        &OngoingEmissionState {
            backstop_allocated: 0,
            backstop_carry: 0,
            backstop_claimed: 0,
            last_distribution: Some(checkpoint),
            pool_allocated: 0,
            pool_carry: 0,
            split_carry: 0,
            total_distributed: 0,
        },
    );
}

#[cfg(test)]
mod tests {
    use mock_emitter::MockEmitter;
    use mock_pool::{MockPool, MockPoolClient};
    use sep_41_token::{testutils::MockTokenClient, TokenClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        vec, Address, Env, Vec,
    };

    use crate::{
        backstop::BackstopTier,
        constants::{Q4W_LOCK_TIME, SCALAR_7},
        dependencies::EmitterClient,
        emissions, storage,
        testutils::{
            create_backstop, create_blnt_token, create_comet_lp_pool, create_mock_pool_factory,
            create_token, create_usdc_token, sync_mock_pool_factory_config,
        },
        BackstopClient,
    };

    use super::*;

    const QUEUE_SECONDS: u64 = 31 * DAY_IN_SECONDS;
    const MAX_BACKFILL_SECONDS: u64 = (MAX_BACKFILLED_EMISSIONS / SCALAR_7) as u64;

    struct Fixture {
        admin: Address,
        backstop: Address,
        blnt: Address,
        blnt_usdc: Address,
        blnt_xlm: Address,
        e: Env,
        emitter: Address,
        incumbent: Address,
        pool: Address,
        user: Address,
    }

    impl Fixture {
        fn create() -> Self {
            let e = Env::default();
            e.mock_all_auths_allowing_non_root_auth();
            e.cost_estimate().budget().reset_unlimited();
            e.ledger().set_timestamp(1_000);

            let backstop = create_backstop(&e);
            let admin = Address::generate(&e);
            let incumbent = Address::generate(&e);
            let user = Address::generate(&e);
            let (blnt, blnt_client) = create_blnt_token(&e, &backstop, &admin);
            let (usdc, usdc_client) = create_usdc_token(&e, &backstop, &admin);
            let (xlm, _) = create_token(&e, &admin);
            let (blnt_usdc, _) = create_comet_lp_pool(&e, &admin, &blnt, &usdc);
            let (blnt_xlm, _) = create_comet_lp_pool(&e, &admin, &blnt, &xlm);
            e.as_contract(&backstop, || {
                storage::set_blnt_usdc_token(&e, &blnt_usdc);
                storage::set_blnt_xlm_token(&e, &blnt_xlm);
            });
            sync_mock_pool_factory_config(&e, &backstop);

            let pool = e.register(MockPool, ());
            MockPoolClient::new(&e, &pool).set_backstop(&backstop);
            let (_, factory) = create_mock_pool_factory(&e, &backstop);
            factory.set_pool(&pool);

            let emitter = e.register(MockEmitter, ());
            let emitter_client = EmitterClient::new(&e, &emitter);
            emitter_client.initialize(&blnt, &incumbent, &blnt_usdc);
            blnt_client.set_admin(&emitter);
            e.as_contract(&backstop, || storage::set_emitter(&e, &emitter));

            TokenClient::new(&e, &blnt_usdc).transfer(&admin, &incumbent, &(10 * SCALAR_7));
            TokenClient::new(&e, &blnt_usdc).transfer(&admin, &user, &(30 * SCALAR_7));
            TokenClient::new(&e, &blnt_xlm).transfer(&admin, &user, &(10 * SCALAR_7));
            usdc_client.mint(&user, &(20 * SCALAR_7));

            let client = BackstopClient::new(&e, &backstop);
            client.deposit(&BackstopTier::SecondLoss, &user, &pool, &(20 * SCALAR_7));
            client.deposit(&BackstopTier::FirstLoss, &user, &pool, &(5 * SCALAR_7));
            e.as_contract(&backstop, || {
                storage::set_reward_zone(&e, &vec![&e, pool.clone()]);
                emissions::refresh_pool_ongoing_assets(&e, &pool);
            });

            Self {
                admin,
                backstop,
                blnt,
                blnt_usdc,
                blnt_xlm,
                e,
                emitter,
                incumbent,
                pool,
                user,
            }
        }

        fn client(&self) -> BackstopClient<'_> {
            BackstopClient::new(&self.e, &self.backstop)
        }

        fn claimable(&self, tier: &BackstopTier, user: &Address, pools: &Vec<Address>) -> i128 {
            self.e.as_contract(&self.backstop, || {
                emissions::preview_user_ongoing_blnt(&self.e, *tier, user, pools)
            })
        }

        fn migration_state(&self) -> MigrationState {
            self.e.as_contract(&self.backstop, || super::state(&self.e))
        }

        fn unlock(&self) -> u64 {
            self.emitter().get_queued_swap().unwrap().unlock_time
        }

        fn emitter(&self) -> EmitterClient<'_> {
            EmitterClient::new(&self.e, &self.emitter)
        }

        fn queue(&self) -> u64 {
            if self.migration_state().migration_epoch_start.is_none() {
                assert_eq!(self.client().distribute(), 0);
            }
            let epoch_start = self.migration_state().migration_epoch_start.unwrap();
            self.emitter()
                .queue_swap_backstop(&self.backstop, &self.blnt_xlm);
            assert_eq!(self.client().distribute(), 0);
            epoch_start
        }

        fn swap_and_sync(&self) {
            self.e.ledger().set_timestamp(self.unlock());
            self.client().distribute();
            self.emitter().swap_backstop();
            assert_eq!(self.client().distribute(), 0);
        }
    }

    #[test]
    fn fresh_emitter_activates_without_a_queued_swap_or_backfill() {
        let fixture = Fixture::create();
        let emitter = fixture.e.register(MockEmitter, ());
        EmitterClient::new(&fixture.e, &emitter).initialize(
            &fixture.blnt,
            &fixture.backstop,
            &fixture.blnt_usdc,
        );
        MockTokenClient::new(&fixture.e, &fixture.blnt).set_admin(&emitter);
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_emitter(&fixture.e, &emitter)
        });

        assert_eq!(fixture.client().distribute(), 0);
        let state = fixture.migration_state();
        assert_eq!(state.status, MigrationStatus::Active);
        assert_eq!(state.migration_epoch_start, None);
        assert_eq!(state.verified_queue_unlock, None);
        assert_eq!(state.scheduled_backfill, 0);
        assert_eq!(state.backfill_end, Some(1_000));

        fixture.e.ledger().set_timestamp(4_600);
        assert_eq!(fixture.client().distribute(), 3_600 * SCALAR_7);
    }

    #[test]
    fn backfill_uses_ordinary_streams_and_exact_funding() {
        let fixture = Fixture::create();
        let discretionary_recipient = Address::generate(&fixture.e);
        let discretionary_amount = 1_000 * SCALAR_7;
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_drop_list(
                &fixture.e,
                &vec![
                    &fixture.e,
                    (discretionary_recipient.clone(), discretionary_amount),
                ],
            );
        });
        assert_eq!(fixture.queue(), 1_000);

        fixture.e.ledger().set_timestamp(1_010);
        let first = fixture.client().distribute();
        assert_eq!(first, 10 * SCALAR_7);
        let pool_state = fixture.e.as_contract(&fixture.backstop, || {
            emissions::get_pool_ongoing_emissions(&fixture.e, &fixture.pool)
        });
        assert!(pool_state.pending_blnt_usdc > 0);
        assert_eq!(pool_state.pending_blnt_xlm, 0);
        assert_eq!(
            fixture.claimable(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
            ),
            0
        );
        assert_eq!(
            fixture.claimable(
                &BackstopTier::FirstLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
            ),
            0
        );
        assert!(fixture
            .client()
            .try_claim(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
                &0,
            )
            .is_err());
        assert_eq!(fixture.client().gulp_emissions(&fixture.pool), 3 * SCALAR_7);

        fixture.swap_and_sync();
        let scheduled = fixture.migration_state().scheduled_backfill;
        assert_eq!(scheduled, QUEUE_SECONDS as i128 * SCALAR_7);
        assert_eq!(fixture.migration_state().funded_backfill, None);
        fixture
            .e
            .ledger()
            .set_timestamp(fixture.migration_state().activated_at.unwrap() + 10);
        assert_eq!(fixture.client().distribute(), 10 * SCALAR_7);
        assert_eq!(
            fixture.claimable(
                &BackstopTier::FirstLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
            ),
            0
        );
        assert!(fixture
            .client()
            .try_claim(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
                &0,
            )
            .is_err());
        assert!(fixture.client().gulp_emissions(&fixture.pool) > 0);

        fixture.client().drop();
        assert_eq!(fixture.migration_state().funded_backfill, Some(scheduled));
        assert!(fixture.migration_state().blnt_binding_verified);
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnt).balance(&fixture.backstop),
            scheduled + 10 * SCALAR_7
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnt).balance(&discretionary_recipient),
            discretionary_amount
        );
        assert!(fixture.client().try_drop().is_err());
        assert!(
            fixture.client().claim(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
                &0,
            ) > 0
        );
        assert!(
            TokenClient::new(&fixture.e, &fixture.blnt_usdc).balance(&fixture.backstop)
                > 20 * SCALAR_7
        );
    }

    #[test]
    fn backfill_distribution_samples_current_weight_like_v2() {
        let fixture = Fixture::create();
        assert_eq!(fixture.client().distribute(), 0);
        fixture.e.ledger().set_timestamp(1_010);

        assert_eq!(
            fixture.client().deposit(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7,
            ),
            SCALAR_7
        );

        assert_eq!(fixture.client().distribute(), 10 * SCALAR_7);
        assert_eq!(
            fixture.claimable(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
            ),
            0
        );
    }

    #[test]
    fn discretionary_drop_does_not_require_positive_backfill() {
        let fixture = Fixture::create();
        let recipient = Address::generate(&fixture.e);
        let amount = 1_000 * SCALAR_7;
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_reward_zone(&fixture.e, &Vec::new(&fixture.e));
            storage::set_drop_list(&fixture.e, &vec![&fixture.e, (recipient.clone(), amount)]);
        });

        fixture.queue();
        fixture.swap_and_sync();
        assert_eq!(fixture.migration_state().scheduled_backfill, 0);

        fixture.client().drop();
        assert_eq!(fixture.migration_state().funded_backfill, Some(0));
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnt).balance(&recipient),
            amount
        );
    }

    #[test]
    fn withdrawal_stops_future_weight_without_forfeiting_accrued_backfill() {
        let fixture = Fixture::create();
        fixture.queue();
        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(fixture.client().gulp_emissions(&fixture.pool), 3 * SCALAR_7);
        fixture.e.ledger().set_timestamp(1_010 + DAY_IN_SECONDS);
        fixture.client().distribute();
        let accrued_before_queue = fixture.claimable(
            &BackstopTier::SecondLoss,
            &fixture.user,
            &vec![&fixture.e, fixture.pool.clone()],
        );
        fixture.client().queue_withdrawal(
            &BackstopTier::SecondLoss,
            &fixture.user,
            &fixture.pool,
            &(20 * SCALAR_7),
        );
        fixture
            .e
            .ledger()
            .set_timestamp(1_010 + DAY_IN_SECONDS + Q4W_LOCK_TIME);
        fixture.client().distribute();
        assert_eq!(
            fixture.client().withdraw(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &fixture.pool,
                &(20 * SCALAR_7),
                &fixture.user,
            ),
            20 * SCALAR_7
        );
        assert_eq!(
            fixture
                .client()
                .user_balance(&BackstopTier::SecondLoss, &fixture.pool, &fixture.user)
                .shares,
            0
        );

        // Preserve the emitter's strict raw-balance qualification without
        // creating replacement tier shares or emission weight.
        TokenClient::new(&fixture.e, &fixture.blnt_usdc).transfer(
            &fixture.admin,
            &fixture.backstop,
            &(11 * SCALAR_7),
        );

        fixture.swap_and_sync();
        assert_eq!(
            fixture.migration_state().scheduled_backfill,
            i128::from(10 + DAY_IN_SECONDS) * SCALAR_7
        );
        fixture.client().drop();
        assert_eq!(
            fixture.claimable(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
            ),
            accrued_before_queue
        );
        assert!(
            fixture.client().claim(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &vec![&fixture.e, fixture.pool.clone()],
                &0,
            ) > 0
        );
        assert!(
            fixture
                .client()
                .user_balance(&BackstopTier::SecondLoss, &fixture.pool, &fixture.user)
                .shares
                > 0
        );
    }

    #[test]
    fn direct_swap_fails_closed_until_distribute_activates() {
        let fixture = Fixture::create();
        fixture.queue();
        let unlock = fixture.unlock();
        fixture
            .e
            .ledger()
            .set_timestamp(unlock - QUEUE_ATTESTATION_WINDOW_SECONDS);
        fixture.client().distribute();
        assert_eq!(
            fixture.migration_state().verified_queue_unlock,
            Some(unlock)
        );
        fixture.e.ledger().set_timestamp(unlock);
        fixture.emitter().swap_backstop();

        assert!(fixture
            .client()
            .try_deposit(
                &BackstopTier::FirstLoss,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            )
            .is_err());
        assert!(fixture
            .client()
            .try_deposit(
                &BackstopTier::SecondLoss,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            )
            .is_err());
        assert_eq!(
            fixture.client().deposit(
                &BackstopTier::ThirdLoss,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            ),
            SCALAR_7
        );

        assert_eq!(fixture.client().distribute(), 0);
        assert_eq!(fixture.migration_state().status, MigrationStatus::Active);
        assert_eq!(
            fixture.migration_state().backfill_end,
            Some(unlock - QUEUE_ATTESTATION_WINDOW_SECONDS)
        );
        assert_eq!(
            fixture.client().deposit(
                &BackstopTier::FirstLoss,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            ),
            SCALAR_7
        );
    }

    #[test]
    fn observed_queue_must_target_the_candidate_and_blnt_xlm() {
        let fixture = Fixture::create();
        assert_eq!(fixture.client().distribute(), 0);
        fixture
            .emitter()
            .queue_swap_backstop(&fixture.backstop, &Address::generate(&fixture.e));
        assert!(fixture.client().try_distribute().is_err());
        let unlock = fixture.unlock();
        fixture.e.ledger().set_timestamp(unlock);
        fixture.emitter().swap_backstop();
        assert!(fixture.client().try_distribute().is_err());
        assert_eq!(fixture.migration_state().status, MigrationStatus::Open);
    }

    #[test]
    fn activation_requires_the_attested_queue_grace_period() {
        let fixture = Fixture::create();
        fixture.queue();
        let unlock = fixture.unlock();
        fixture
            .e
            .ledger()
            .set_timestamp(unlock - QUEUE_ATTESTATION_WINDOW_SECONDS);
        fixture.client().distribute();
        fixture.e.ledger().set_timestamp(unlock);
        fixture.emitter().swap_backstop();
        fixture
            .e
            .ledger()
            .set_timestamp(unlock + ACTIVATION_GRACE_SECONDS + 1);

        assert!(fixture.client().try_distribute().is_err());
        assert_eq!(fixture.migration_state().status, MigrationStatus::Open);
    }

    #[test]
    fn expired_live_queue_clears_its_attestation_before_swap() {
        let fixture = Fixture::create();
        fixture.queue();
        let unlock = fixture.unlock();
        fixture.e.ledger().set_timestamp(unlock);
        fixture.client().distribute();
        assert_eq!(
            fixture.migration_state().verified_queue_unlock,
            Some(unlock)
        );

        fixture
            .e
            .ledger()
            .set_timestamp(unlock + ACTIVATION_GRACE_SECONDS + 1);
        fixture.client().distribute();
        assert_eq!(fixture.migration_state().verified_queue_unlock, None);
    }

    #[test]
    fn replacement_queue_does_not_reset_or_pause_the_backfill_clock() {
        let fixture = Fixture::create();
        let epoch_start = fixture.queue();
        let original_unlock = fixture.unlock();
        let token = TokenClient::new(&fixture.e, &fixture.blnt_usdc);

        fixture.e.ledger().set_timestamp(original_unlock);
        let incumbent_top_up = token.balance(&fixture.backstop) - token.balance(&fixture.incumbent);
        token.transfer(&fixture.admin, &fixture.incumbent, &incumbent_top_up);
        fixture.emitter().cancel_swap_backstop();
        token.transfer(&fixture.admin, &fixture.backstop, &1);
        fixture
            .emitter()
            .queue_swap_backstop(&fixture.backstop, &fixture.blnt_xlm);
        let replacement_unlock = fixture.emitter().get_queued_swap().unwrap().unlock_time;
        fixture
            .e
            .ledger()
            .set_timestamp(original_unlock + 10 * DAY_IN_SECONDS);
        assert_eq!(
            fixture.client().distribute(),
            (original_unlock + 10 * DAY_IN_SECONDS - epoch_start) as i128 * SCALAR_7
        );
        assert_eq!(
            fixture.migration_state().scheduled_backfill,
            (original_unlock + 10 * DAY_IN_SECONDS - epoch_start) as i128 * SCALAR_7
        );

        fixture.e.ledger().set_timestamp(replacement_unlock);
        fixture.swap_and_sync();
        assert_eq!(
            fixture.migration_state().backfill_end,
            Some(replacement_unlock)
        );
    }

    #[test]
    fn delayed_queue_reaches_the_full_backfill_cap() {
        let fixture = Fixture::create();
        let epoch_start = fixture.e.ledger().timestamp();
        assert_eq!(fixture.client().distribute(), 0);

        let queue_start = epoch_start + MAX_BACKFILL_SECONDS - QUEUE_SECONDS;
        fixture.e.ledger().set_timestamp(queue_start);
        assert_eq!(
            fixture.client().distribute(),
            (MAX_BACKFILL_SECONDS - QUEUE_SECONDS) as i128 * SCALAR_7
        );
        fixture
            .emitter()
            .queue_swap_backstop(&fixture.backstop, &fixture.blnt_xlm);
        assert_eq!(fixture.client().distribute(), 0);

        fixture.swap_and_sync();
        let state = fixture.migration_state();
        assert_eq!(state.scheduled_backfill, MAX_BACKFILLED_EMISSIONS);
        assert_eq!(state.backfill_end, Some(epoch_start + MAX_BACKFILL_SECONDS));
        fixture.client().drop();
        assert_eq!(
            fixture.migration_state().funded_backfill,
            Some(MAX_BACKFILLED_EMISSIONS)
        );
    }

    #[test]
    fn pre_swap_lifecycle_has_no_local_deadline_and_remains_capped() {
        let fixture = Fixture::create();
        fixture.e.ledger().set_timestamp(1_000 + QUEUE_SECONDS + 1);
        assert_eq!(fixture.client().distribute(), 0);
        let epoch_start = fixture.migration_state().migration_epoch_start.unwrap();

        fixture
            .e
            .ledger()
            .set_timestamp(epoch_start + MAX_BACKFILL_SECONDS + DAY_IN_SECONDS);
        assert_eq!(fixture.client().distribute(), MAX_BACKFILLED_EMISSIONS);
        fixture
            .emitter()
            .queue_swap_backstop(&fixture.backstop, &fixture.blnt_xlm);
        assert_eq!(fixture.client().distribute(), 0);
        assert_eq!(
            fixture.migration_state().scheduled_backfill,
            MAX_BACKFILLED_EMISSIONS
        );
    }
}
