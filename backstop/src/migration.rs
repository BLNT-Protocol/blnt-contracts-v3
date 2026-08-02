use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Vec};

use crate::{
    constants::MAX_BACKFILLED_EMISSIONS,
    dependencies::{EmitterClient, Swap},
    emissions,
    errors::BackstopError,
    events::BackstopEvents,
    storage::{self, OngoingEmissionState},
};

const DAY_IN_SECONDS: u64 = 24 * 60 * 60;
const QUEUE_SECONDS: u64 = 31 * DAY_IN_SECONDS;
const PREPARATION_WINDOW_SECONDS: u64 = 7 * DAY_IN_SECONDS;
const SYNC_GRACE_SECONDS: u64 = 7 * DAY_IN_SECONDS;
const RETRY_PERIOD_SECONDS: u64 = QUEUE_SECONDS + SYNC_GRACE_SECONDS;
const MAX_RETRY_QUEUES: u32 = 2;
const RECOVERY_HORIZON_SECONDS: u64 = MAX_RETRY_QUEUES as u64 * RETRY_PERIOD_SECONDS;
const MAX_MIGRATION_LIFETIME_SECONDS: u64 = 2 * QUEUE_SECONDS + RECOVERY_HORIZON_SECONDS;

#[derive(Clone)]
#[contracttype]
enum MigrationDataKey {
    PrefundingStart,
    AbsoluteMigrationDeadline,
    MigrationEpochStart,
    OriginalUnlock,
    VerifiedQueueUnlock,
    RetryCount,
    ActivatedAt,
    BackfillEnd,
    ScheduledBackfill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum MigrationStatus {
    Pending,
    Open,
    Prepared,
    Active,
}

pub(crate) fn initialize(e: &Env) {
    let prefunding_start = e.ledger().timestamp();
    let absolute_deadline =
        checked_add_seconds(e, prefunding_start, MAX_MIGRATION_LIFETIME_SECONDS);
    e.storage()
        .instance()
        .set(&MigrationDataKey::PrefundingStart, &prefunding_start);
    e.storage().instance().set(
        &MigrationDataKey::AbsoluteMigrationDeadline,
        &absolute_deadline,
    );
}

pub(crate) fn is_active(e: &Env) -> bool {
    activated_at(e).is_some()
}

pub(crate) fn require_active(e: &Env) {
    if !is_active(e) {
        panic_with_error!(e, BackstopError::MigrationNotActive);
    }
}

/// Fail closed while the emitter points to a prepared candidate whose local
/// lifecycle has not yet synchronized.
pub(crate) fn require_weight_mutation_allowed(e: &Env) {
    if is_active(e) {
        return;
    }
    let Some(verified_unlock) = verified_queue_unlock(e) else {
        return;
    };
    if e.ledger().timestamp() < verified_unlock {
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

pub(crate) fn unfunded_backfill(e: &Env) -> i128 {
    let scheduled = scheduled_backfill(e);
    match storage::get_backfill_funded_amount(e) {
        None => scheduled,
        Some(funded) if funded == scheduled => 0,
        Some(_) => panic_with_error!(e, BackstopError::InvalidBackfillFunding),
    }
}

pub(crate) fn status(e: &Env) -> MigrationStatus {
    if is_active(e) {
        MigrationStatus::Active
    } else if verified_queue_unlock(e).is_some() {
        MigrationStatus::Prepared
    } else if original_unlock(e).is_some() {
        MigrationStatus::Open
    } else {
        MigrationStatus::Pending
    }
}

pub(crate) fn prefunding_start(e: &Env) -> u64 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::PrefundingStart)
        .unwrap()
}

pub(crate) fn absolute_migration_deadline(e: &Env) -> u64 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::AbsoluteMigrationDeadline)
        .unwrap()
}

pub(crate) fn migration_epoch_start(e: &Env) -> Option<u64> {
    e.storage()
        .instance()
        .get(&MigrationDataKey::MigrationEpochStart)
}

pub(crate) fn original_unlock(e: &Env) -> Option<u64> {
    e.storage()
        .instance()
        .get(&MigrationDataKey::OriginalUnlock)
}

pub(crate) fn verified_queue_unlock(e: &Env) -> Option<u64> {
    e.storage()
        .instance()
        .get(&MigrationDataKey::VerifiedQueueUnlock)
}

pub(crate) fn retry_count(e: &Env) -> u32 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::RetryCount)
        .unwrap_or(0)
}

pub(crate) fn activated_at(e: &Env) -> Option<u64> {
    e.storage().instance().get(&MigrationDataKey::ActivatedAt)
}

pub(crate) fn backfill_end(e: &Env) -> Option<u64> {
    e.storage().instance().get(&MigrationDataKey::BackfillEnd)
}

pub(crate) fn backfill_cap(e: &Env) -> Option<u64> {
    original_unlock(e).map(|unlock| recovery_end(e, unlock))
}

pub(crate) fn sync_deadline(e: &Env) -> Option<u64> {
    verified_queue_unlock(e).map(|unlock| checked_sync_deadline(e, unlock))
}

pub(crate) fn scheduled_backfill(e: &Env) -> i128 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::ScheduledBackfill)
        .unwrap_or(0)
}

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

pub(crate) fn backfill_checkpoint(e: &Env) -> u64 {
    require_not_active(e);
    let queued = emitter(e)
        .get_queued_swap()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::SwapNotQueued));
    require_valid_queue(e, &queued);
    e.ledger().timestamp().min(queued.unlock_time)
}

pub(crate) fn fund_backfill(e: &Env) -> i128 {
    if storage::get_backfill_funded_amount(e).is_some() {
        panic_with_error!(e, BackstopError::BackfillAlreadyFunded);
    }
    require_active(e);
    let scheduled = scheduled_backfill(e);
    if scheduled <= 0 || scheduled > MAX_BACKFILLED_EMISSIONS {
        panic_with_error!(e, BackstopError::InvalidBackfillFunding);
    }

    // Persist before the external call so reentrancy or repetition cannot
    // create two obligations. A later failure rolls this marker back.
    storage::set_backfill_funded_amount(e, scheduled);
    let candidate = e.current_contract_address();
    let blnd = TokenClient::new(e, &storage::get_blnd_token(e));
    let balance_before = blnd.balance(&candidate);
    let mut recipients = Vec::new(e);
    recipients.push_back((candidate.clone(), scheduled));
    emitter(e).drop(&recipients);
    let received = checked_signed_sub(e, blnd.balance(&candidate), balance_before);
    if received != scheduled {
        panic_with_error!(e, BackstopError::InvalidBackfillFunding);
    }
    storage::set_blnd_binding_verified(e);
    BackstopEvents::backfill_funded(e, scheduled);
    scheduled
}

pub(crate) fn open_migration_epoch(e: &Env) -> u64 {
    require_not_active(e);
    if let Some(epoch_start) = migration_epoch_start(e) {
        return epoch_start;
    }
    let queued = emitter(e)
        .get_queued_swap()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::SwapNotQueued));
    require_valid_queue(e, &queued);
    open_epoch(e, queued.unlock_time)
}

pub(crate) fn begin_migration(e: &Env) -> u64 {
    require_not_active(e);
    if original_unlock(e).is_some() {
        panic_with_error!(e, BackstopError::MigrationEpochAlreadyOpen);
    }
    let client = emitter(e);
    let candidate = e.current_contract_address();
    client.queue_swap_backstop(&candidate, &storage::get_blnd_xlm_token(e));
    let queued = client
        .get_queued_swap()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::SwapNotQueued));
    require_valid_queue(e, &queued);
    open_epoch(e, queued.unlock_time)
}

pub(crate) fn prepare_migration(e: &Env) -> u64 {
    require_not_active(e);
    let client = emitter(e);
    let candidate = e.current_contract_address();
    if client.get_backstop() == candidate {
        panic_with_error!(e, BackstopError::EmitterAlreadyMigrated);
    }
    let queued = client
        .get_queued_swap()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::SwapNotQueued));
    require_valid_queue(e, &queued);
    require_candidate_balance_majority(e, &client, &candidate);

    let now = e.ledger().timestamp();
    let preparation_start = queued
        .unlock_time
        .saturating_sub(PREPARATION_WINDOW_SECONDS);
    if now < preparation_start || now > queued.unlock_time {
        panic_with_error!(e, BackstopError::PreparationOutsideWindow);
    }
    let original = original_unlock(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationEpochNotOpen));

    if let Some(verified) = verified_queue_unlock(e) {
        if verified == queued.unlock_time {
            return original;
        }
        let retries = retry_count(e);
        if retries >= MAX_RETRY_QUEUES {
            panic_with_error!(e, BackstopError::RetryLimitExceeded);
        }
        if checked_sync_deadline(e, queued.unlock_time) > recovery_end(e, original) {
            panic_with_error!(e, BackstopError::RecoveryHorizonExceeded);
        }
        let next_retries = retries
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        e.storage()
            .instance()
            .set(&MigrationDataKey::VerifiedQueueUnlock, &queued.unlock_time);
        e.storage()
            .instance()
            .set(&MigrationDataKey::RetryCount, &next_retries);
        BackstopEvents::migration_prepared(e, original, next_retries, queued.unlock_time);
        return original;
    }

    let retries = if queued.unlock_time == original {
        0
    } else {
        if checked_sync_deadline(e, queued.unlock_time) > recovery_end(e, original) {
            panic_with_error!(e, BackstopError::RecoveryHorizonExceeded);
        }
        1
    };
    e.storage()
        .instance()
        .set(&MigrationDataKey::VerifiedQueueUnlock, &queued.unlock_time);
    e.storage()
        .instance()
        .set(&MigrationDataKey::RetryCount, &retries);
    BackstopEvents::migration_prepared(e, original, retries, queued.unlock_time);
    original
}

pub(crate) fn finalize_migration(e: &Env) -> u64 {
    require_not_active(e);
    let original = require_prepared(e);
    if e.ledger().timestamp() > recovery_end(e, original) {
        panic_with_error!(e, BackstopError::RecoveryHorizonExceeded);
    }
    let client = emitter(e);
    let candidate = e.current_contract_address();
    if client.get_backstop() == candidate {
        panic_with_error!(e, BackstopError::EmitterAlreadyMigrated);
    }
    let queued = client
        .get_queued_swap()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::SwapNotQueued));
    require_valid_queue(e, &queued);
    require_verified_queue(e, &queued);
    require_candidate_balance_majority(e, &client, &candidate);
    client.swap_backstop();
    if client.get_backstop() != candidate {
        panic_with_error!(e, BackstopError::EmitterDidNotMigrate);
    }
    activate(e, e.ledger().timestamp())
}

pub(crate) fn sync_migration(e: &Env) -> u64 {
    require_not_active(e);
    require_prepared(e);
    let verified = verified_queue_unlock(e).unwrap();
    if e.ledger().timestamp() > checked_sync_deadline(e, verified) {
        panic_with_error!(e, BackstopError::SyncWindowExpired);
    }
    if emitter(e).get_backstop() != e.current_contract_address() {
        panic_with_error!(e, BackstopError::EmitterDidNotMigrate);
    }
    // The emitter exposes no immutable direct-swap timestamp after another
    // distribution, so the verified unlock is the conservative cutoff.
    activate(e, verified)
}

fn activate(e: &Env, requested_backfill_end: u64) -> u64 {
    let original = require_prepared(e);
    let activated = e.ledger().timestamp();
    if requested_backfill_end > activated {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let end = requested_backfill_end.min(recovery_end(e, original));
    emissions::checkpoint_backfill(e, end);

    let last_distribution = emitter(e).get_last_distro(&e.current_contract_address());
    if last_distribution > activated {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let mut state = storage::get_ongoing_emission_state(e);
    state.last_distribution = Some(last_distribution);
    storage::set_ongoing_emission_state(e, &state);
    e.storage()
        .instance()
        .set(&MigrationDataKey::ActivatedAt, &activated);
    e.storage()
        .instance()
        .set(&MigrationDataKey::BackfillEnd, &end);
    BackstopEvents::migration_activated(e, activated, end);
    activated
}

fn emitter(e: &Env) -> EmitterClient<'_> {
    EmitterClient::new(e, &storage::get_emitter(e))
}

fn require_not_active(e: &Env) {
    if is_active(e) {
        panic_with_error!(e, BackstopError::AlreadyFinalized);
    }
}

fn require_prepared(e: &Env) -> u64 {
    if verified_queue_unlock(e).is_none() {
        panic_with_error!(e, BackstopError::MigrationNotPrepared);
    }
    original_unlock(e).unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationNotPrepared))
}

fn require_candidate_balance_majority(e: &Env, client: &EmitterClient<'_>, candidate: &Address) {
    let incumbent = client.get_backstop();
    let token = TokenClient::new(e, &storage::get_blnd_usdc_token(e));
    if token.balance(candidate) <= token.balance(&incumbent) {
        panic_with_error!(e, BackstopError::InsufficientFunds);
    }
}

fn require_valid_queue(e: &Env, queued: &Swap) {
    if queued.new_backstop != e.current_contract_address()
        || queued.new_backstop_token != storage::get_blnd_xlm_token(e)
    {
        panic_with_error!(e, BackstopError::InvalidQueuedSwap);
    }
}

fn require_verified_queue(e: &Env, queued: &Swap) {
    if verified_queue_unlock(e) != Some(queued.unlock_time) {
        panic_with_error!(e, BackstopError::QueueNotVerified);
    }
}

fn open_epoch(e: &Env, unlock: u64) -> u64 {
    let epoch_start = unlock
        .checked_sub(QUEUE_SECONDS)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let duration = epoch_start
        .checked_sub(prefunding_start(e))
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if duration > QUEUE_SECONDS {
        panic_with_error!(e, BackstopError::PrefundingWindowExceeded);
    }
    let now = e.ledger().timestamp();
    if now < epoch_start || now > unlock {
        panic_with_error!(e, BackstopError::PreparationOutsideWindow);
    }
    recovery_end(e, unlock);
    e.storage()
        .instance()
        .set(&MigrationDataKey::MigrationEpochStart, &epoch_start);
    e.storage()
        .instance()
        .set(&MigrationDataKey::OriginalUnlock, &unlock);
    e.storage()
        .instance()
        .set(&MigrationDataKey::RetryCount, &0_u32);
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
            total_claimed: 0,
            total_distributed: 0,
        },
    );
    storage::set_reward_zone_checkpoint(e, epoch_start);
    storage::set_reward_zone_distribution_started(e);
    epoch_start
}

fn checked_sync_deadline(e: &Env, unlock: u64) -> u64 {
    checked_add_seconds(e, unlock, SYNC_GRACE_SECONDS)
}

fn recovery_end(e: &Env, original: u64) -> u64 {
    let queue_end = checked_add_seconds(e, original, RECOVERY_HORIZON_SECONDS);
    if queue_end > absolute_migration_deadline(e) {
        panic_with_error!(e, BackstopError::RecoveryHorizonExceeded);
    }
    queue_end
}

fn checked_add_seconds(e: &Env, timestamp: u64, seconds: u64) -> u64 {
    timestamp
        .checked_add(seconds)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_signed_sub(e: &Env, lhs: i128, rhs: i128) -> i128 {
    lhs.checked_sub(rhs)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

#[cfg(test)]
pub(crate) fn activate_for_test(e: &Env, checkpoint: u64) {
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
            total_claimed: 0,
            total_distributed: 0,
        },
    );
}

#[cfg(test)]
mod tests {
    use mock_emitter::MockEmitter;
    use mock_pool::{MockPool, MockPoolClient};
    use sep_41_token::TokenClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        vec, Address, Env,
    };

    use crate::{
        backstop::BackstopTier,
        constants::{Q4W_LOCK_TIME, SCALAR_7},
        dependencies::EmitterClient,
        emissions, storage,
        testutils::{
            create_backstop, create_blnd_token, create_comet_lp_pool, create_mock_pool_factory,
            create_token, create_usdc_token,
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
            let (blnd, blnd_client) = create_blnd_token(&e, &backstop, &admin);
            let (usdc, usdc_client) = create_usdc_token(&e, &backstop, &admin);
            let (xlm, _) = create_token(&e, &admin);
            let (blnd_usdc, _) = create_comet_lp_pool(&e, &admin, &blnd, &usdc);
            let (blnd_xlm, _) = create_comet_lp_pool(&e, &admin, &blnd, &xlm);
            e.as_contract(&backstop, || {
                storage::set_blnd_usdc_token(&e, &blnd_usdc);
                storage::set_blnd_xlm_token(&e, &blnd_xlm);
            });

            let pool = e.register(MockPool, ());
            MockPoolClient::new(&e, &pool).set_backstop(&backstop);
            let (_, factory) = create_mock_pool_factory(&e, &backstop);
            factory.set_mock_pool(&pool);

            let emitter = e.register(MockEmitter, ());
            let emitter_client = EmitterClient::new(&e, &emitter);
            emitter_client.initialize(&blnd, &incumbent, &blnd_usdc);
            blnd_client.set_admin(&emitter);
            e.as_contract(&backstop, || storage::set_emitter(&e, &emitter));

            TokenClient::new(&e, &blnd_usdc).transfer(&admin, &incumbent, &(10 * SCALAR_7));
            TokenClient::new(&e, &blnd_usdc).transfer(&admin, &user, &(30 * SCALAR_7));
            TokenClient::new(&e, &blnd_xlm).transfer(&admin, &user, &(10 * SCALAR_7));
            usdc_client.mint(&user, &(20 * SCALAR_7));

            let client = BackstopClient::new(&e, &backstop);
            client.deposit(&BackstopTier::BlndUsdc, &user, &pool, &(20 * SCALAR_7));
            client.deposit(&BackstopTier::BlndXlm, &user, &pool, &(5 * SCALAR_7));
            e.as_contract(&backstop, || {
                storage::set_reward_zone(&e, &vec![&e, pool.clone()]);
                emissions::refresh_pool_ongoing_assets(&e, &pool);
            });

            Self {
                admin,
                backstop,
                blnd,
                blnd_usdc,
                blnd_xlm,
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

        fn unlock(&self) -> u64 {
            self.client().original_unlock().unwrap()
        }

        fn emitter(&self) -> EmitterClient<'_> {
            EmitterClient::new(&self.e, &self.emitter)
        }

        fn prepare(&self) {
            self.e
                .ledger()
                .set_timestamp(self.unlock() - PREPARATION_WINDOW_SECONDS);
            self.client().prepare_migration();
        }

        fn finalize(&self) {
            self.e.ledger().set_timestamp(self.unlock());
            self.client().finalize_migration();
        }

        fn prepare_and_finalize(&self) {
            self.prepare();
            self.finalize();
        }
    }

    #[test]
    fn backfill_uses_ordinary_indexes_and_exact_funding() {
        let fixture = Fixture::create();
        assert_eq!(fixture.client().begin_migration(), 1_000);

        fixture.e.ledger().set_timestamp(1_010);
        let first = fixture.client().distribute();
        assert_eq!(first.distributed, 10 * SCALAR_7);
        assert_eq!(first.backstop_allocated, 7 * SCALAR_7);
        assert_eq!(first.pool_allocated, 3 * SCALAR_7);
        let pool_state = fixture.client().pool_ongoing_emissions(&fixture.pool);
        assert!(pool_state.blnd_usdc_index > 0);
        assert_eq!(pool_state.blnd_xlm_index, 0);
        assert_eq!(
            fixture
                .client()
                .user_ongoing_emissions(&fixture.user, &fixture.pool, &BackstopTier::BlndUsdc,)
                .accrued,
            7 * SCALAR_7
        );
        assert_eq!(
            fixture
                .client()
                .user_ongoing_emissions(&fixture.user, &fixture.pool, &BackstopTier::BlndXlm,)
                .accrued,
            0
        );
        assert!(fixture
            .client()
            .try_claim_ongoing_blnd(&BackstopTier::BlndUsdc, &fixture.user, &fixture.pool, &0,)
            .is_err());

        fixture.prepare_and_finalize();
        let scheduled = fixture.client().scheduled_backfill();
        assert_eq!(scheduled, QUEUE_SECONDS as i128 * SCALAR_7);
        assert_eq!(fixture.client().funded_backfill(), None);
        fixture.e.ledger().set_timestamp(fixture.unlock() + 10);
        assert_eq!(fixture.client().distribute().distributed, 10 * SCALAR_7);
        assert!(
            fixture
                .client()
                .user_ongoing_emissions(&fixture.user, &fixture.pool, &BackstopTier::BlndXlm,)
                .accrued
                > 0
        );
        assert!(fixture
            .client()
            .try_claim_ongoing_blnd(&BackstopTier::BlndUsdc, &fixture.user, &fixture.pool, &0,)
            .is_err());

        assert_eq!(fixture.client().fund_backfill(), scheduled);
        assert_eq!(fixture.client().funded_backfill(), Some(scheduled));
        assert!(fixture.client().blnd_binding_verified());
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            scheduled + 10 * SCALAR_7
        );
        assert!(fixture.client().try_fund_backfill().is_err());
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.backstop),
            20 * SCALAR_7
        );
    }

    #[test]
    fn withdrawal_stops_future_weight_without_forfeiting_accrued_backfill() {
        let fixture = Fixture::create();
        fixture.client().begin_migration();
        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        let accrued_before_queue = fixture
            .client()
            .user_ongoing_emissions(&fixture.user, &fixture.pool, &BackstopTier::BlndUsdc)
            .accrued;
        fixture.client().queue_withdrawal(
            &BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &(20 * SCALAR_7),
        );
        fixture.e.ledger().set_timestamp(1_010 + Q4W_LOCK_TIME);
        fixture.client().distribute();
        assert_eq!(
            fixture.client().withdraw(
                &BackstopTier::BlndUsdc,
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
                .tier_shares(&BackstopTier::BlndUsdc, &fixture.user, &fixture.pool,),
            0
        );

        // Preserve the emitter's strict raw-balance qualification without
        // creating replacement tier shares or emission weight.
        TokenClient::new(&fixture.e, &fixture.blnd_usdc).transfer(
            &fixture.admin,
            &fixture.backstop,
            &(11 * SCALAR_7),
        );

        fixture.prepare_and_finalize();
        assert_eq!(fixture.client().scheduled_backfill(), 10 * SCALAR_7);
        fixture.client().fund_backfill();
        assert_eq!(
            fixture
                .client()
                .user_ongoing_emissions(&fixture.user, &fixture.pool, &BackstopTier::BlndUsdc,)
                .accrued,
            accrued_before_queue
        );
        assert!(
            fixture.client().claim_ongoing_blnd(
                &BackstopTier::BlndUsdc,
                &fixture.user,
                &fixture.pool,
                &0,
            ) > 0
        );
        assert!(
            fixture
                .client()
                .tier_shares(&BackstopTier::BlndUsdc, &fixture.user, &fixture.pool,)
                > 0
        );
    }

    #[test]
    fn direct_swap_fails_closed_until_permissionless_synchronization() {
        let fixture = Fixture::create();
        fixture.client().begin_migration();
        fixture.prepare();
        let unlock = fixture.unlock();
        fixture.e.ledger().set_timestamp(unlock);
        fixture.emitter().swap_backstop();

        assert!(fixture
            .client()
            .try_deposit(
                &BackstopTier::BlndXlm,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            )
            .is_err());
        assert!(fixture
            .client()
            .try_deposit(
                &BackstopTier::BlndUsdc,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            )
            .is_err());
        assert_eq!(
            fixture
                .client()
                .deposit(&BackstopTier::Usdc, &fixture.user, &fixture.pool, &SCALAR_7),
            SCALAR_7
        );

        assert_eq!(fixture.client().sync_migration(), unlock);
        assert_eq!(fixture.client().migration_status(), MigrationStatus::Active);
        assert_eq!(fixture.client().backfill_end(), Some(unlock));
        assert_eq!(
            fixture.client().deposit(
                &BackstopTier::BlndXlm,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            ),
            SCALAR_7
        );
    }

    #[test]
    fn preparation_rechecks_the_strict_legacy_token_majority() {
        let fixture = Fixture::create();
        fixture.client().begin_migration();
        fixture.client().queue_withdrawal(
            &BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &(20 * SCALAR_7),
        );
        fixture
            .e
            .ledger()
            .set_timestamp(1_000 + 17 * DAY_IN_SECONDS);
        fixture.client().distribute();
        fixture.client().withdraw(
            &BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &(20 * SCALAR_7),
            &fixture.user,
        );

        fixture
            .e
            .ledger()
            .set_timestamp(fixture.unlock() - PREPARATION_WINDOW_SECONDS);
        assert!(fixture.client().try_prepare_migration().is_err());
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.incumbent),
            10 * SCALAR_7
        );
    }

    #[test]
    fn two_replacement_queues_preserve_the_original_horizon() {
        let fixture = Fixture::create();
        let epoch_start = fixture.client().begin_migration();
        fixture.prepare();
        let original_unlock = fixture.unlock();
        let token = TokenClient::new(&fixture.e, &fixture.blnd_usdc);

        fixture.e.ledger().set_timestamp(original_unlock);
        let incumbent_top_up = token.balance(&fixture.backstop) - token.balance(&fixture.incumbent);
        token.transfer(&fixture.admin, &fixture.incumbent, &incumbent_top_up);
        fixture.emitter().cancel_swap_backstop();
        token.transfer(&fixture.admin, &fixture.backstop, &1);
        fixture
            .emitter()
            .queue_swap_backstop(&fixture.backstop, &fixture.blnd_xlm);
        let first_replacement_unlock = fixture.emitter().get_queued_swap().unwrap().unlock_time;
        fixture
            .e
            .ledger()
            .set_timestamp(first_replacement_unlock - PREPARATION_WINDOW_SECONDS);
        fixture.client().prepare_migration();
        assert_eq!(fixture.client().retry_count(), 1);

        fixture
            .e
            .ledger()
            .set_timestamp(first_replacement_unlock + SYNC_GRACE_SECONDS);
        let incumbent_top_up =
            token.balance(&fixture.backstop) - token.balance(&fixture.incumbent) + 1;
        token.transfer(&fixture.admin, &fixture.incumbent, &incumbent_top_up);
        fixture.emitter().cancel_swap_backstop();
        let candidate_top_up =
            token.balance(&fixture.incumbent) - token.balance(&fixture.backstop) + 1;
        token.transfer(&fixture.admin, &fixture.backstop, &candidate_top_up);
        fixture
            .emitter()
            .queue_swap_backstop(&fixture.backstop, &fixture.blnd_xlm);
        let second_replacement_unlock = fixture.emitter().get_queued_swap().unwrap().unlock_time;
        fixture
            .e
            .ledger()
            .set_timestamp(second_replacement_unlock - PREPARATION_WINDOW_SECONDS);
        fixture.client().prepare_migration();
        assert_eq!(fixture.client().retry_count(), MAX_RETRY_QUEUES);
        assert_eq!(
            second_replacement_unlock,
            original_unlock + RECOVERY_HORIZON_SECONDS - SYNC_GRACE_SECONDS
        );

        fixture
            .e
            .ledger()
            .set_timestamp(original_unlock + RECOVERY_HORIZON_SECONDS);
        fixture.client().finalize_migration();
        assert_eq!(fixture.client().scheduled_backfill(), 9_244_800 * SCALAR_7);
        assert_eq!(
            fixture.client().backfill_end(),
            Some(epoch_start + 107 * DAY_IN_SECONDS)
        );
        assert_eq!(
            fixture.client().backfill_cap(),
            Some(original_unlock + RECOVERY_HORIZON_SECONDS)
        );
    }
}
