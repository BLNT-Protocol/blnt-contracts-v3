use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Vec, I256};

use crate::{
    constants::{MAX_BACKFILLED_EMISSIONS, SCALAR_7},
    dependencies::{EmitterClient, Swap},
    emissions::OngoingEmissionState,
    errors::BackstopError,
    events::BackstopEvents,
    storage,
};

const DAY_IN_LEDGERS: u32 = 17_280;
const POSITION_TTL_THRESHOLD: u32 = 179 * DAY_IN_LEDGERS;
const POSITION_TTL_BUMP: u32 = 180 * DAY_IN_LEDGERS - 1;
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
    TotalMigrationAmount,
    TotalMigrationTimeBasis,
    TotalMigrationWeight,
    ScheduledBackfill,
}

#[derive(Clone)]
#[contracttype]
enum MigrationPersistentKey {
    Position(Address, Address),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum MigrationStatus {
    Pending,
    Open,
    Prepared,
    Active,
}

/// One continuous BLND:USDC tier position plus temporary migration-backfill metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct MigrationPosition {
    /// Tier shares currently owned by this user for this pool.
    pub amount: i128,
    /// Actual contributed LP tokens that remain eligible for migration backfill.
    pub backfill_amount: i128,
    pub backfill_claimed: bool,
    pub frozen_weight: Option<i128>,
    /// Sum of each eligible deposit amount multiplied by its deposit timestamp.
    pub time_basis: i128,
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

pub(crate) fn require_active(e: &Env) {
    if !e.storage().instance().has(&MigrationDataKey::ActivatedAt) {
        panic_with_error!(e, BackstopError::MigrationNotActive);
    }
}

/// Fail closed while the emitter points to a prepared candidate whose local
/// lifecycle has not yet synchronized.
pub(crate) fn require_weight_mutation_allowed(e: &Env) {
    if e.storage().instance().has(&MigrationDataKey::ActivatedAt) {
        return;
    }
    let Some(verified_unlock) = e
        .storage()
        .instance()
        .get::<MigrationDataKey, u64>(&MigrationDataKey::VerifiedQueueUnlock)
    else {
        return;
    };
    if e.ledger().timestamp() < verified_unlock {
        return;
    }
    if emitter(e).get_backstop() == e.current_contract_address() {
        panic_with_error!(e, BackstopError::MigrationNotActive);
    }
}

pub(crate) fn record_blnd_usdc_deposit(
    e: &Env,
    user: &Address,
    pool: &Address,
    assets: i128,
    shares: i128,
) {
    if assets <= 0 || shares <= 0 {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }
    let mut position = get_position(e, user, pool);
    if let Some(backfill_end) = backfill_end_value(e) {
        freeze_position_weight(e, &mut position, backfill_end);
    } else {
        try_open_epoch_from_current_queue(e);
        if deposit_eligible_for_current_queue(e) {
            let timestamp = eligibility_timestamp(e, e.ledger().timestamp());
            let basis_delta = checked_mul(e, assets, timestamp as i128);
            position.backfill_amount = checked_add(e, position.backfill_amount, assets);
            position.time_basis = checked_add(e, position.time_basis, basis_delta);
            add_total(e, MigrationDataKey::TotalMigrationAmount, assets);
            add_total(e, MigrationDataKey::TotalMigrationTimeBasis, basis_delta);
        }
    }
    position.amount = checked_add(e, position.amount, shares);
    set_position(e, user, pool, &position);
}

pub(crate) fn record_blnd_usdc_withdrawal(e: &Env, user: &Address, pool: &Address, shares: i128) {
    if shares <= 0 {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }
    let mut position = get_position(e, user, pool);
    if shares > position.amount {
        panic_with_error!(e, BackstopError::BalanceError);
    }
    if let Some(backfill_end) = backfill_end_value(e) {
        freeze_position_weight(e, &mut position, backfill_end);
    } else {
        let principal_removed =
            proportional_floor(e, position.backfill_amount, shares, position.amount);
        let basis_removed = proportional_floor(e, position.time_basis, shares, position.amount);
        position.backfill_amount = checked_sub(e, position.backfill_amount, principal_removed);
        position.time_basis = checked_sub(e, position.time_basis, basis_removed);
        add_total(
            e,
            MigrationDataKey::TotalMigrationAmount,
            -principal_removed,
        );
        add_total(e, MigrationDataKey::TotalMigrationTimeBasis, -basis_removed);
    }
    position.amount = checked_sub(e, position.amount, shares);
    set_position(e, user, pool, &position);
}

pub(crate) fn status(e: &Env) -> MigrationStatus {
    if e.storage().instance().has(&MigrationDataKey::ActivatedAt) {
        MigrationStatus::Active
    } else if e
        .storage()
        .instance()
        .has(&MigrationDataKey::VerifiedQueueUnlock)
    {
        MigrationStatus::Prepared
    } else if e
        .storage()
        .instance()
        .has(&MigrationDataKey::OriginalUnlock)
    {
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
    backfill_end_value(e)
}

pub(crate) fn backfill_cap(e: &Env) -> Option<u64> {
    original_unlock(e).map(|unlock| recovery_end(e, unlock))
}

pub(crate) fn sync_deadline(e: &Env) -> Option<u64> {
    verified_queue_unlock(e).map(|unlock| checked_sync_deadline(e, unlock))
}

pub(crate) fn position(e: &Env, user: &Address, pool: &Address) -> MigrationPosition {
    get_position(e, user, pool)
}

pub(crate) fn position_weight_read(e: &Env, user: &Address, pool: &Address) -> i128 {
    let end = backfill_end_value(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationNotActive));
    let position = get_position(e, user, pool);
    position
        .frozen_weight
        .unwrap_or_else(|| calculate_position_weight(e, &position, end))
}

pub(crate) fn total_migration_weight(e: &Env) -> i128 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::TotalMigrationWeight)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationNotActive))
}

pub(crate) fn scheduled_backfill(e: &Env) -> i128 {
    e.storage()
        .instance()
        .get(&MigrationDataKey::ScheduledBackfill)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationNotActive))
}

pub(crate) fn funded_backfill(e: &Env) -> Option<i128> {
    storage::get_backfill_funded_amount(e)
}

pub(crate) fn total_backfill_claimed(e: &Env) -> i128 {
    storage::get_total_backfill_claimed(e)
}

pub(crate) fn remaining_backfill(e: &Env) -> i128 {
    let funded = storage::get_backfill_funded_amount(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::BackfillNotFunded));
    checked_sub(e, funded, storage::get_total_backfill_claimed(e))
}

pub(crate) fn quote_backfill(e: &Env, user: &Address, pool: &Address) -> i128 {
    let total_weight = total_migration_weight(e);
    if total_weight <= 0 {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }
    proportional_floor(
        e,
        scheduled_backfill(e),
        position_weight_read(e, user, pool),
        total_weight,
    )
}

pub(crate) fn fund_backfill(e: &Env) -> i128 {
    if storage::get_backfill_funded_amount(e).is_some() {
        panic_with_error!(e, BackstopError::BackfillAlreadyFunded);
    }
    require_active(e);
    let scheduled = scheduled_backfill(e);
    let total_weight = total_migration_weight(e);
    if scheduled <= 0 || scheduled > MAX_BACKFILLED_EMISSIONS {
        panic_with_error!(e, BackstopError::InvalidBackfillFunding);
    }
    if total_weight <= 0 {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }

    // Persist before the external call so a reentrant or repeated call cannot
    // create two obligations. Any later failure rolls this marker back.
    storage::set_backfill_funded_amount(e, scheduled);
    storage::set_total_backfill_claimed(e, 0);

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

pub(crate) fn claim_backfill(e: &Env, user: &Address, pool: &Address, recipient: &Address) -> i128 {
    user.require_auth();
    let funded = storage::get_backfill_funded_amount(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::BackfillNotFunded));
    let total_weight = total_migration_weight(e);
    if total_weight <= 0 {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }
    let end = backfill_end_value(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationNotActive));
    let mut position = get_position(e, user, pool);
    if position.backfill_claimed {
        panic_with_error!(e, BackstopError::BackfillAlreadyClaimed);
    }
    let weight = position
        .frozen_weight
        .unwrap_or_else(|| calculate_position_weight(e, &position, end));
    if weight <= 0 {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }
    let amount = proportional_floor(e, funded, weight, total_weight);
    if amount <= 0 {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }
    let next_claimed = checked_add(e, storage::get_total_backfill_claimed(e), amount);
    if next_claimed > funded {
        panic_with_error!(e, BackstopError::OverflowError);
    }

    position.frozen_weight = Some(weight);
    position.backfill_claimed = true;
    set_position(e, user, pool, &position);
    storage::set_total_backfill_claimed(e, next_claimed);
    TokenClient::new(e, &storage::get_blnd_token(e)).transfer(
        &e.current_contract_address(),
        recipient,
        &amount,
    );
    BackstopEvents::backfill_claimed(e, user.clone(), pool.clone(), recipient.clone(), amount);
    amount
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
    // The emitter exposes no immutable direct-swap timestamp. The verified
    // unlock is therefore the conservative recovery-path backfill cutoff.
    activate(e, verified)
}

fn activate(e: &Env, requested_backfill_end: u64) -> u64 {
    let original = require_prepared(e);
    let activated = e.ledger().timestamp();
    if requested_backfill_end > activated {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let client = emitter(e);
    let last_distribution = client.get_last_distro(&e.current_contract_address());
    if last_distribution > activated {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let end = requested_backfill_end.min(recovery_end(e, original));
    let epoch_start = migration_epoch_start(e).unwrap();
    let scheduled = calculate_scheduled_backfill(e, epoch_start, end);
    let total_amount = get_total(e, MigrationDataKey::TotalMigrationAmount);
    let total_basis = get_total(e, MigrationDataKey::TotalMigrationTimeBasis);
    let total_weight = checked_sub(e, checked_mul(e, total_amount, end as i128), total_basis);

    e.storage()
        .instance()
        .set(&MigrationDataKey::ActivatedAt, &activated);
    e.storage()
        .instance()
        .set(&MigrationDataKey::BackfillEnd, &end);
    e.storage()
        .instance()
        .set(&MigrationDataKey::ScheduledBackfill, &scheduled);
    e.storage()
        .instance()
        .set(&MigrationDataKey::TotalMigrationWeight, &total_weight);
    storage::set_ongoing_emission_state(
        e,
        &OngoingEmissionState {
            backstop_allocated: 0,
            backstop_carry: 0,
            backstop_claimed: 0,
            last_distribution: Some(last_distribution),
            pool_allocated: 0,
            pool_carry: 0,
            split_carry: 0,
            total_claimed: 0,
            total_received: 0,
        },
    );
    BackstopEvents::migration_activated(e, activated, end);
    activated
}

fn emitter(e: &Env) -> EmitterClient<'_> {
    EmitterClient::new(e, &storage::get_emitter(e))
}

fn require_not_active(e: &Env) {
    if activated_at(e).is_some() {
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

fn try_open_epoch_from_current_queue(e: &Env) {
    if migration_epoch_start(e).is_some() {
        return;
    }
    let Some(queued) = emitter(e).get_queued_swap() else {
        return;
    };
    if queued.new_backstop == e.current_contract_address()
        && queued.new_backstop_token == storage::get_blnd_xlm_token(e)
        && e.ledger().timestamp() <= queued.unlock_time
    {
        open_epoch(e, queued.unlock_time);
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

fn eligibility_timestamp(e: &Env, timestamp: u64) -> u64 {
    original_unlock(e)
        .map(|unlock| timestamp.min(recovery_end(e, unlock)))
        .unwrap_or(timestamp)
}

fn deposit_eligible_for_current_queue(e: &Env) -> bool {
    verified_queue_unlock(e).is_none_or(|unlock| e.ledger().timestamp() < unlock)
}

fn backfill_end_value(e: &Env) -> Option<u64> {
    e.storage().instance().get(&MigrationDataKey::BackfillEnd)
}

fn freeze_position_weight(e: &Env, position: &mut MigrationPosition, end: u64) {
    if position.frozen_weight.is_none() {
        position.frozen_weight = Some(calculate_position_weight(e, position, end));
    }
}

fn get_position(e: &Env, user: &Address, pool: &Address) -> MigrationPosition {
    let key = MigrationPersistentKey::Position(user.clone(), pool.clone());
    let value = e
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(MigrationPosition {
            amount: 0,
            backfill_amount: 0,
            backfill_claimed: false,
            frozen_weight: None,
            time_basis: 0,
        });
    if e.storage().persistent().has(&key) {
        e.storage()
            .persistent()
            .extend_ttl(&key, POSITION_TTL_THRESHOLD, POSITION_TTL_BUMP);
    }
    value
}

fn set_position(e: &Env, user: &Address, pool: &Address, position: &MigrationPosition) {
    let key = MigrationPersistentKey::Position(user.clone(), pool.clone());
    e.storage().persistent().set(&key, position);
    e.storage()
        .persistent()
        .extend_ttl(&key, POSITION_TTL_THRESHOLD, POSITION_TTL_BUMP);
}

fn get_total(e: &Env, key: MigrationDataKey) -> i128 {
    e.storage().instance().get(&key).unwrap_or(0)
}

fn add_total(e: &Env, key: MigrationDataKey, delta: i128) {
    let next = checked_add(e, get_total(e, key.clone()), delta);
    if next < 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    e.storage().instance().set(&key, &next);
}

fn calculate_position_weight(e: &Env, position: &MigrationPosition, end: u64) -> i128 {
    checked_sub(
        e,
        checked_mul(e, position.backfill_amount, end as i128),
        position.time_basis,
    )
}

fn calculate_scheduled_backfill(e: &Env, start: u64, end: u64) -> i128 {
    let seconds = end
        .checked_sub(start)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    checked_mul(e, seconds as i128, SCALAR_7).min(MAX_BACKFILLED_EMISSIONS)
}

fn proportional_floor(e: &Env, value: i128, numerator: i128, denominator: i128) -> i128 {
    if value < 0 || numerator < 0 || denominator <= 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    I256::from_i128(e, value)
        .mul(&I256::from_i128(e, numerator))
        .div(&I256::from_i128(e, denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_add_seconds(e: &Env, timestamp: u64, seconds: u64) -> u64 {
    timestamp
        .checked_add(seconds)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_add(e: &Env, lhs: i128, rhs: i128) -> i128 {
    lhs.checked_add(rhs)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_sub(e: &Env, lhs: i128, rhs: i128) -> i128 {
    let value = lhs
        .checked_sub(rhs)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    if value < 0 {
        panic_with_error!(e, BackstopError::OverflowError);
    }
    value
}

fn checked_signed_sub(e: &Env, lhs: i128, rhs: i128) -> i128 {
    lhs.checked_sub(rhs)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_mul(e: &Env, lhs: i128, rhs: i128) -> i128 {
    lhs.checked_mul(rhs)
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
            total_received: 0,
        },
    );
}

#[cfg(test)]
mod tests {
    use mock_emitter::MockEmitter;
    use mock_pool::{MockPool, MockPoolClient};
    use sep_41_token::testutils::MockTokenClient;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env,
    };

    use crate::{
        dependencies::EmitterClient,
        storage,
        testutils::{
            create_backstop, create_backstop_token, create_blnd_token, create_blnd_xlm_token,
            create_mock_pool_factory, create_usdc_token,
        },
        BackstopClient,
    };

    use super::*;

    struct Fixture {
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
            let (blnd_usdc, _) = create_backstop_token(&e, &backstop, &admin);
            let (blnd_xlm, _) = create_blnd_xlm_token(&e, &backstop, &admin);
            let (usdc, _) = create_usdc_token(&e, &backstop, &admin);
            let pool = e.register(MockPool, ());
            MockPoolClient::new(&e, &pool).set_backstop(&backstop);
            let (_, factory) = create_mock_pool_factory(&e, &backstop);
            factory.set_mock_pool(&pool);

            let emitter = e.register(MockEmitter, ());
            let emitter_client = EmitterClient::new(&e, &emitter);
            emitter_client.initialize(&blnd, &incumbent, &blnd_usdc);
            blnd_client.set_admin(&emitter);
            e.as_contract(&backstop, || storage::set_emitter(&e, &emitter));

            MockTokenClient::new(&e, &blnd_usdc).mint(&incumbent, &(10 * SCALAR_7));
            MockTokenClient::new(&e, &blnd_usdc).mint(&user, &(200 * SCALAR_7));
            MockTokenClient::new(&e, &blnd_xlm).mint(&user, &(20 * SCALAR_7));
            MockTokenClient::new(&e, &usdc).mint(&user, &(20 * SCALAR_7));

            Self {
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

        fn emitter(&self) -> EmitterClient<'_> {
            EmitterClient::new(&self.e, &self.emitter)
        }

        fn unlock(&self) -> u64 {
            self.client().original_unlock().unwrap()
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
    }

    #[test]
    fn canonical_lifecycle_funds_and_claims_only_scheduled_backfill() {
        let fixture = Fixture::create();
        let amount = 20 * SCALAR_7;
        assert_eq!(
            fixture.client().deposit(
                &crate::BackstopTier::BlndUsdc,
                &fixture.user,
                &fixture.pool,
                &amount
            ),
            amount
        );
        assert_eq!(
            fixture
                .client()
                .migration_position(&fixture.user, &fixture.pool),
            MigrationPosition {
                amount,
                backfill_amount: amount,
                backfill_claimed: false,
                frozen_weight: None,
                time_basis: amount * 1_000,
            }
        );

        assert_eq!(fixture.client().begin_migration(), 1_000);
        let unlock = fixture.unlock();
        assert_eq!(unlock, 1_000 + QUEUE_SECONDS);
        assert_eq!(fixture.client().migration_status(), MigrationStatus::Open);
        assert!(fixture.client().try_prepare_migration().is_err());

        fixture.prepare();
        assert_eq!(
            fixture.client().migration_status(),
            MigrationStatus::Prepared
        );
        fixture.finalize();
        assert_eq!(fixture.client().migration_status(), MigrationStatus::Active);
        assert_eq!(fixture.emitter().get_backstop(), fixture.backstop);
        assert_eq!(fixture.client().backfill_end(), Some(unlock));
        assert_eq!(
            fixture.client().scheduled_backfill(),
            QUEUE_SECONDS as i128 * SCALAR_7
        );
        assert_eq!(
            fixture.client().total_migration_weight(),
            amount * QUEUE_SECONDS as i128
        );
        assert_eq!(fixture.client().funded_backfill(), None);

        let scheduled = fixture.client().fund_backfill();
        assert_eq!(scheduled, QUEUE_SECONDS as i128 * SCALAR_7);
        assert_eq!(fixture.client().funded_backfill(), Some(scheduled));
        assert_eq!(
            MockTokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            scheduled
        );
        assert!(fixture.client().blnd_binding_verified());

        let recipient = Address::generate(&fixture.e);
        assert_eq!(
            fixture
                .client()
                .claim_backfill(&fixture.user, &fixture.pool, &recipient),
            scheduled
        );
        assert_eq!(
            MockTokenClient::new(&fixture.e, &fixture.blnd).balance(&recipient),
            scheduled
        );
        assert_eq!(fixture.client().total_backfill_claimed(), scheduled);
        assert_eq!(fixture.client().remaining_backfill(), 0);
        assert!(fixture
            .client()
            .try_claim_backfill(&fixture.user, &fixture.pool, &recipient)
            .is_err());
        assert!(fixture.client().try_fund_backfill().is_err());
    }

    #[test]
    fn actual_early_withdrawal_forfeits_principal_and_time_basis_pro_rata() {
        let fixture = Fixture::create();
        let amount = 100 * SCALAR_7;
        let withdrawn = 40 * SCALAR_7;
        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &amount,
        );
        fixture.client().begin_migration();
        fixture.client().queue_withdrawal(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &withdrawn,
        );
        assert_eq!(
            fixture
                .client()
                .migration_position(&fixture.user, &fixture.pool)
                .backfill_amount,
            amount
        );
        fixture
            .e
            .ledger()
            .set_timestamp(1_000 + 17 * DAY_IN_SECONDS);
        assert_eq!(
            fixture.client().withdraw(
                &crate::BackstopTier::BlndUsdc,
                &fixture.user,
                &fixture.pool,
                &withdrawn,
                &fixture.user,
            ),
            withdrawn
        );

        let surviving = amount - withdrawn;
        assert_eq!(
            fixture
                .client()
                .migration_position(&fixture.user, &fixture.pool),
            MigrationPosition {
                amount: surviving,
                backfill_amount: surviving,
                backfill_claimed: false,
                frozen_weight: None,
                time_basis: surviving * 1_000,
            }
        );
        fixture.prepare();
        fixture.finalize();
        assert_eq!(
            fixture
                .client()
                .migration_weight(&fixture.user, &fixture.pool),
            surviving * QUEUE_SECONDS as i128
        );

        // The sole surviving position receives the full scheduled amount;
        // withdrawn weight is absent from both numerator and denominator.
        let funded = fixture.client().fund_backfill();
        assert_eq!(
            fixture
                .client()
                .quote_backfill(&fixture.user, &fixture.pool),
            funded
        );
    }

    #[test]
    fn direct_swap_fails_closed_until_permissionless_synchronization() {
        let fixture = Fixture::create();
        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &(20 * SCALAR_7),
        );
        fixture.client().begin_migration();
        fixture.prepare();
        let unlock = fixture.unlock();
        fixture.e.ledger().set_timestamp(unlock);
        fixture.emitter().swap_backstop();

        assert!(fixture
            .client()
            .try_deposit(
                &crate::BackstopTier::BlndXlm,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            )
            .is_err());
        assert!(fixture
            .client()
            .try_deposit(
                &crate::BackstopTier::BlndUsdc,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            )
            .is_err());
        assert_eq!(
            fixture.client().deposit(
                &crate::BackstopTier::Usdc,
                &fixture.user,
                &fixture.pool,
                &SCALAR_7
            ),
            SCALAR_7
        );

        assert_eq!(fixture.client().sync_migration(), unlock);
        assert_eq!(fixture.client().migration_status(), MigrationStatus::Active);
        assert_eq!(fixture.client().backfill_end(), Some(unlock));
        assert_eq!(
            fixture.client().deposit(
                &crate::BackstopTier::BlndXlm,
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
        let amount = 20 * SCALAR_7;
        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &amount,
        );
        fixture.client().begin_migration();
        fixture.client().queue_withdrawal(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &amount,
        );
        fixture
            .e
            .ledger()
            .set_timestamp(1_000 + 17 * DAY_IN_SECONDS);
        fixture.client().withdraw(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &amount,
            &fixture.user,
        );
        fixture
            .e
            .ledger()
            .set_timestamp(fixture.unlock() - PREPARATION_WINDOW_SECONDS);
        assert!(fixture.client().try_prepare_migration().is_err());
        assert_eq!(
            MockTokenClient::new(&fixture.e, &fixture.blnd_usdc).balance(&fixture.incumbent),
            10 * SCALAR_7
        );
    }

    #[test]
    fn deposit_at_verified_unlock_is_ordinary_tier_capital_only() {
        let fixture = Fixture::create();
        let eligible = 20 * SCALAR_7;
        let ordinary = 5 * SCALAR_7;
        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &eligible,
        );
        fixture.client().begin_migration();
        fixture.prepare();

        fixture.e.ledger().set_timestamp(fixture.unlock());
        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &ordinary,
        );
        fixture.client().finalize_migration();

        assert_eq!(
            fixture
                .client()
                .migration_position(&fixture.user, &fixture.pool),
            MigrationPosition {
                amount: eligible + ordinary,
                backfill_amount: eligible,
                backfill_claimed: false,
                frozen_weight: None,
                time_basis: eligible * 1_000,
            }
        );
        assert_eq!(
            fixture
                .client()
                .migration_weight(&fixture.user, &fixture.pool),
            eligible * QUEUE_SECONDS as i128
        );
    }

    #[test]
    fn two_replacement_queues_preserve_the_original_horizon() {
        let fixture = Fixture::create();
        let amount = 20 * SCALAR_7;
        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.user,
            &fixture.pool,
            &amount,
        );
        let epoch_start = fixture.client().begin_migration();
        fixture.prepare();
        let original_unlock = fixture.unlock();
        let token = MockTokenClient::new(&fixture.e, &fixture.blnd_usdc);

        fixture.e.ledger().set_timestamp(original_unlock);
        let incumbent_top_up =
            token.balance(&fixture.backstop) - token.balance(&fixture.incumbent) + 1;
        token.mint(&fixture.incumbent, &incumbent_top_up);
        fixture.emitter().cancel_swap_backstop();
        let candidate_top_up =
            token.balance(&fixture.incumbent) - token.balance(&fixture.backstop) + 1;
        token.mint(&fixture.backstop, &candidate_top_up);
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
        token.mint(&fixture.incumbent, &incumbent_top_up);
        fixture.emitter().cancel_swap_backstop();
        let candidate_top_up =
            token.balance(&fixture.incumbent) - token.balance(&fixture.backstop) + 1;
        token.mint(&fixture.backstop, &candidate_top_up);
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
            fixture.client().total_migration_weight(),
            amount * (original_unlock + RECOVERY_HORIZON_SECONDS - epoch_start) as i128
        );
        assert_eq!(
            fixture.client().backfill_cap(),
            Some(original_unlock + RECOVERY_HORIZON_SECONDS)
        );
    }
}
