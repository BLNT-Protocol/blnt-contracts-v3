use sep_41_token::TokenClient;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contracttype, panic_with_error, vec, Address, Env, IntoVal, Map, Symbol, Val, Vec, I256,
};

use crate::{
    backstop::{
        credit_tier_shares, require_registered_pool, tier_token, BackstopTier, BlndEmissionValues,
    },
    constants::{MAX_BACKFILLED_EMISSIONS, SCALAR_14, SCALAR_7},
    dependencies::{CometClient, EmitterClient},
    errors::BackstopError,
    migration,
    storage::{
        self, OngoingEmissionState, PoolOngoingEmissions, TierEmissionStream, UserOngoingEmissions,
    },
};

use super::policy::{
    comet_composition, pool_active_emission_assets, proportional_floor, quote_ongoing_blnd_split,
    underlying_blnd_from_composition,
};

const MIN_DISTRIBUTION_INTERVAL_SECONDS: u64 = 5;
const WEIGHT_CHANGE_CHECKPOINT_MAX_AGE_SECONDS: u64 = 5;
const POOL_EMISSION_GULP_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
const BACKSTOP_EMISSION_STREAM_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Result of one completed permissionless BLND distribution checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct OngoingDistribution {
    pub backstop_allocated: i128,
    pub backstop_carry: i128,
    pub checkpoint: u64,
    pub eligible_blnd: i128,
    pub pool_allocated: i128,
    pub pool_carry: i128,
    pub distributed: i128,
    pub split_carry: i128,
}

pub(crate) struct OngoingClaim {
    pub lp_amount: i128,
    pub allocations: Vec<(Address, i128, i128, i128)>,
}

pub(crate) fn distribute(e: &Env) -> OngoingDistribution {
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

/// Accrue migration backfill through the ordinary 70/30 emission pipeline.
/// Before activation, active BLND:USDC LP tokens directly determine pool
/// weight, and only that tier receives pending backstop emissions.
pub(crate) fn checkpoint_backfill(e: &Env, checkpoint: u64) -> OngoingDistribution {
    if migration::is_active(e) {
        panic_with_error!(e, BackstopError::AlreadyFinalized);
    }
    let mut state = get_ongoing_emission_state(e);
    let last_distribution = state
        .last_distribution
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::MigrationEpochNotOpen));
    let elapsed = checkpoint
        .checked_sub(last_distribution)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidOngoingBalance));
    let (weights, total_eligible_blnd) = collect_weights(e, false);
    if total_eligible_blnd == 0 {
        return advance_without_distribution(e, &mut state, checkpoint);
    }
    let accrued = emissions_for_seconds(e, elapsed);
    let remaining = checked_sub(
        e,
        MAX_BACKFILLED_EMISSIONS,
        migration::scheduled_backfill(e),
    );
    let amount = accrued.min(remaining);
    let result = allocate_distribution(
        e,
        &mut state,
        amount,
        checkpoint,
        weights,
        total_eligible_blnd,
    );
    migration::record_backfill_distribution(e, amount);
    result
}

fn collect_weights(
    e: &Env,
    use_underlying_blnd: bool,
) -> (Vec<(Address, i128, BlndEmissionValues)>, i128) {
    let reward_zone = storage::get_reward_zone(e);
    if reward_zone.is_empty() {
        return (Vec::new(e), 0);
    }
    let (blnd_usdc_supply, blnd_usdc_reserve) = if use_underlying_blnd {
        comet_composition(e, BackstopTier::BlndUsdc)
    } else {
        (0, 0)
    };
    let (blnd_xlm_supply, blnd_xlm_reserve) = if use_underlying_blnd {
        comet_composition(e, BackstopTier::BlndXlm)
    } else {
        (0, 0)
    };
    let mut weights = Vec::new(e);
    let mut total_eligible_blnd = 0_i128;
    let mut total_blnd_usdc = 0_i128;
    let mut total_blnd_xlm = 0_i128;
    for pool in reward_zone.iter() {
        let pool_state = get_pool_ongoing_emissions(e, &pool);
        let pool_blnd_usdc = if use_underlying_blnd {
            total_blnd_usdc = checked_add(e, total_blnd_usdc, pool_state.active_blnd_usdc);
            underlying_blnd_from_composition(
                e,
                pool_state.active_blnd_usdc,
                blnd_usdc_supply,
                blnd_usdc_reserve,
            )
        } else {
            pool_state.active_blnd_usdc
        };
        let pool_blnd_xlm = if use_underlying_blnd {
            total_blnd_xlm = checked_add(e, total_blnd_xlm, pool_state.active_blnd_xlm);
            underlying_blnd_from_composition(
                e,
                pool_state.active_blnd_xlm,
                blnd_xlm_supply,
                blnd_xlm_reserve,
            )
        } else {
            0
        };
        let pool_weight = checked_add(e, pool_blnd_usdc, pool_blnd_xlm);
        total_eligible_blnd = checked_add(e, total_eligible_blnd, pool_weight);
        weights.push_back((
            pool,
            pool_weight,
            BlndEmissionValues {
                blnd_usdc: pool_blnd_usdc,
                blnd_xlm: pool_blnd_xlm,
            },
        ));
    }
    if use_underlying_blnd
        && (total_blnd_usdc > blnd_usdc_supply || total_blnd_xlm > blnd_xlm_supply)
    {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }
    (weights, total_eligible_blnd)
}

fn allocate_distribution(
    e: &Env,
    state: &mut OngoingEmissionState,
    amount: i128,
    checkpoint: u64,
    weights: Vec<(Address, i128, BlndEmissionValues)>,
    total_eligible_blnd: i128,
) -> OngoingDistribution {
    let split = quote_ongoing_blnd_split(e, amount, state.split_carry);
    let backstop_distribution = checked_add(e, split.backstop, state.backstop_carry);
    let pool_distribution = checked_add(e, split.pool, state.pool_carry);
    let mut backstop_allocated = 0_i128;
    let mut pool_allocated = 0_i128;
    for (pool, weight, values) in weights.iter() {
        let (backstop_allocation, pool_allocation) = if total_eligible_blnd == 0 {
            (0, 0)
        } else {
            (
                proportional_floor(e, backstop_distribution, weight, total_eligible_blnd),
                proportional_floor(e, pool_distribution, weight, total_eligible_blnd),
            )
        };
        let mut pool_state = get_pool_ongoing_emissions(e, &pool);
        allocate_pool_backstop_emissions(e, &mut pool_state, &values, backstop_allocation);
        pool_state.accrued_backstop =
            checked_add(e, pool_state.accrued_backstop, backstop_allocation);
        pool_state.accrued_pool = checked_add(e, pool_state.accrued_pool, pool_allocation);
        set_pool_ongoing_emissions(e, &pool, &pool_state);
        backstop_allocated = checked_add(e, backstop_allocated, backstop_allocation);
        pool_allocated = checked_add(e, pool_allocated, pool_allocation);
    }

    state.total_distributed = checked_add(e, state.total_distributed, amount);
    state.backstop_allocated = checked_add(e, state.backstop_allocated, backstop_allocated);
    state.pool_allocated = checked_add(e, state.pool_allocated, pool_allocated);
    state.split_carry = split.carry;
    state.backstop_carry = checked_sub(e, backstop_distribution, backstop_allocated);
    state.pool_carry = checked_sub(e, pool_distribution, pool_allocated);
    state.last_distribution = Some(checkpoint);
    set_ongoing_emission_state(e, state);
    storage::set_reward_zone_checkpoint(e, checkpoint);
    storage::set_reward_zone_distribution_started(e);

    OngoingDistribution {
        backstop_allocated,
        backstop_carry: state.backstop_carry,
        checkpoint,
        eligible_blnd: total_eligible_blnd,
        pool_allocated,
        pool_carry: state.pool_carry,
        distributed: amount,
        split_carry: state.split_carry,
    }
}

fn advance_without_distribution(
    e: &Env,
    state: &mut OngoingEmissionState,
    checkpoint: u64,
) -> OngoingDistribution {
    state.last_distribution = Some(checkpoint);
    set_ongoing_emission_state(e, state);
    storage::set_reward_zone_checkpoint(e, checkpoint);
    storage::set_reward_zone_distribution_started(e);
    OngoingDistribution {
        backstop_allocated: 0,
        backstop_carry: state.backstop_carry,
        checkpoint,
        eligible_blnd: 0,
        pool_allocated: 0,
        pool_carry: state.pool_carry,
        distributed: 0,
        split_carry: state.split_carry,
    }
}

fn get_ongoing_emission_state(e: &Env) -> OngoingEmissionState {
    let state = storage::get_ongoing_emission_state(e);
    validate_ongoing_emission_state(e, &state);
    state
}

pub(crate) fn get_pool_ongoing_emissions(e: &Env, pool: &Address) -> PoolOngoingEmissions {
    let state = storage::get_pool_ongoing_emissions(e, pool);
    validate_pool_ongoing_emissions(e, &state);
    state
}

pub(crate) fn refresh_pool_ongoing_assets(e: &Env, pool: &Address) {
    let mut state = get_pool_ongoing_emissions(e, pool);
    let blnd_usdc = storage::get_pool_balance_for_tier(e, BackstopTier::BlndUsdc, pool);
    state.active_blnd_usdc_shares = checked_sub(e, blnd_usdc.shares, blnd_usdc.q4w);
    state.active_blnd_usdc = pool_active_emission_assets(e, BackstopTier::BlndUsdc, pool);
    let blnd_xlm = storage::get_pool_balance_for_tier(e, BackstopTier::BlndXlm, pool);
    state.active_blnd_xlm_shares = checked_sub(e, blnd_xlm.shares, blnd_xlm.q4w);
    state.active_blnd_xlm = pool_active_emission_assets(e, BackstopTier::BlndXlm, pool);
    set_pool_ongoing_emissions(e, pool, &state);
}

pub(crate) fn preview_user_ongoing_emissions(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
) -> UserOngoingEmissions {
    require_emission_tier(e, tier);
    let mut pool_state = get_pool_ongoing_emissions(e, pool);
    let current_index = advance_pool_tier_stream(e, &mut pool_state, tier);
    accrue_user_ongoing_emissions(
        e,
        get_user_ongoing_emissions(e, tier, user, pool),
        storage::get_user_balance_for_tier(e, tier, pool, user).shares,
        current_index,
    )
}

pub(crate) fn preview_user_ongoing_blnd(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool_addresses: &Vec<Address>,
) -> i128 {
    require_emission_tier(e, tier);
    if pool_addresses.is_empty() {
        panic_with_error!(e, BackstopError::BadRequest);
    }

    let mut claimable = 0_i128;
    let mut pools = Map::<Address, ()>::new(e);
    for pool in pool_addresses.iter() {
        if pools.contains_key(pool.clone()) {
            panic_with_error!(e, BackstopError::BadRequest);
        }
        require_registered_pool(e, &pool);
        pools.set(pool.clone(), ());
        claimable = checked_add(
            e,
            claimable,
            preview_user_ongoing_emissions(e, tier, user, &pool).accrued,
        );
    }
    claimable
}

pub(crate) fn checkpoint_user_ongoing_for_weight_change(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
) {
    if tier != BackstopTier::Usdc {
        checkpoint_user_ongoing_emissions(e, tier, user, pool);
    }
}

pub(crate) fn claim_user_ongoing_blnd(
    e: &Env,
    tier: BackstopTier,
    from: &Address,
    pool_addresses: &Vec<Address>,
    min_lp_tokens_out: i128,
) -> OngoingClaim {
    migration::require_backfill_funded(e);
    from.require_auth();
    require_emission_tier(e, tier);
    if pool_addresses.is_empty() {
        panic_with_error!(e, BackstopError::BadRequest);
    }
    if min_lp_tokens_out < 0 {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }

    let mut blnd_amount = 0_i128;
    let mut claims = Map::<Address, i128>::new(e);
    for pool in pool_addresses.iter() {
        if claims.contains_key(pool.clone()) {
            panic_with_error!(e, BackstopError::BadRequest);
        }
        require_registered_pool(e, &pool);
        prepare_pool_weight_change(e, tier, &pool);

        let mut user_emissions = checkpoint_user_ongoing_emissions(e, tier, from, &pool);
        let pool_claim = user_emissions.accrued;
        claims.set(pool.clone(), pool_claim);
        blnd_amount = checked_add(e, blnd_amount, pool_claim);
        if pool_claim == 0 {
            continue;
        }

        user_emissions.accrued = 0;
        set_user_ongoing_emissions(e, tier, from, &pool, &user_emissions);

        let mut pool_state = get_pool_ongoing_emissions(e, &pool);
        pool_state.accrued_backstop = checked_sub(e, pool_state.accrued_backstop, pool_claim);
        set_pool_ongoing_emissions(e, &pool, &pool_state);
    }

    if blnd_amount == 0 {
        return OngoingClaim {
            lp_amount: 0,
            allocations: vec![e],
        };
    }

    let mut ongoing = get_ongoing_emission_state(e);
    ongoing.backstop_claimed = checked_add(e, ongoing.backstop_claimed, blnd_amount);
    set_ongoing_emission_state(e, &ongoing);

    let backstop = e.current_contract_address();
    let blnd = storage::get_blnd_token(e);
    let lp_token = tier_token(e, tier);
    let blnd_client = TokenClient::new(e, &blnd);
    let lp_client = TokenClient::new(e, &lp_token);
    let blnd_before = blnd_client.balance(&backstop);
    let lp_before = lp_client.balance(&backstop);
    let approval_ledger = e
        .ledger()
        .sequence()
        .checked_div(100_000)
        .and_then(|period| period.checked_add(1))
        .and_then(|period| period.checked_mul(100_000))
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let approval_args: Vec<Val> = vec![
        e,
        backstop.clone().into_val(e),
        lp_token.clone().into_val(e),
        blnd_amount.into_val(e),
        approval_ledger.into_val(e),
    ];
    e.authorize_as_current_contract(vec![
        e,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: blnd.clone(),
                fn_name: Symbol::new(e, "approve"),
                args: approval_args,
            },
            sub_invocations: vec![e],
        }),
    ]);
    let lp_amount = CometClient::new(e, &lp_token).dep_tokn_amt_in_get_lp_tokns_out(
        &blnd,
        &blnd_amount,
        &min_lp_tokens_out,
        &backstop,
    );
    let blnd_after = blnd_client.balance(&backstop);
    let lp_after = lp_client.balance(&backstop);
    if blnd_before.checked_sub(blnd_after) != Some(blnd_amount)
        || lp_after.checked_sub(lp_before) != Some(lp_amount)
        || lp_amount <= 0
    {
        panic_with_error!(e, BackstopError::BalanceError);
    }

    let mut allocations = vec![e];
    for pool in pool_addresses.iter() {
        let pool_claim = claims.get(pool.clone()).unwrap_or(0);
        let pool_lp_amount = proportional_floor(e, lp_amount, pool_claim, blnd_amount);
        if pool_lp_amount == 0 {
            continue;
        }
        let shares = credit_tier_shares(e, tier, from, &pool, pool_lp_amount);
        finish_pool_weight_change(e, tier, &pool);
        allocations.push_back((pool, pool_claim, pool_lp_amount, shares));
    }
    OngoingClaim {
        lp_amount,
        allocations,
    }
}

pub(crate) fn gulp_pool_ongoing_emissions(e: &Env, pool: &Address) -> (i128, i128) {
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
    refresh_tier_stream(
        e,
        &mut pool_state.blnd_usdc_stream,
        pool_state.active_blnd_usdc_shares,
        pool_state.pending_blnd_usdc,
        now,
    );
    refresh_tier_stream(
        e,
        &mut pool_state.blnd_xlm_stream,
        pool_state.active_blnd_xlm_shares,
        pool_state.pending_blnd_xlm,
        now,
    );
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

pub(crate) fn prepare_pool_weight_change(e: &Env, tier: BackstopTier, pool: &Address) {
    if tier == BackstopTier::Usdc {
        return;
    }
    migration::require_weight_mutation_allowed(e);
    if tier == BackstopTier::BlndXlm && !migration::is_active(e) {
        return;
    }
    if !storage::get_reward_zone(e).contains(pool.clone())
        || get_ongoing_emission_state(e).last_distribution.is_none()
    {
        return;
    }
    let now = e.ledger().timestamp();
    let checkpoint = storage::get_reward_zone_checkpoint(e)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::DistributionCheckpointRequired));
    if checkpoint > now
        || now
            .checked_sub(checkpoint)
            .is_none_or(|age| age > WEIGHT_CHANGE_CHECKPOINT_MAX_AGE_SECONDS)
    {
        panic_with_error!(e, BackstopError::DistributionCheckpointRequired);
    }
}

pub(crate) fn finish_pool_weight_change(e: &Env, tier: BackstopTier, pool: &Address) {
    // A removed pool can still have a previously scheduled seven-day stream.
    // Keep its cached active shares synchronized until that stream expires so
    // later dequeues or deposits cannot distort the remaining distribution.
    if tier != BackstopTier::Usdc {
        refresh_pool_ongoing_assets(e, pool);
    }
}

fn require_emission_tier(e: &Env, tier: BackstopTier) {
    if tier == BackstopTier::Usdc {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn get_user_ongoing_emissions(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
) -> UserOngoingEmissions {
    require_emission_tier(e, tier);
    let state = storage::get_user_ongoing_emissions(e, tier, user, pool);
    validate_user_ongoing_emissions(e, &state);
    state
}

fn set_user_ongoing_emissions(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
    state: &UserOngoingEmissions,
) {
    require_emission_tier(e, tier);
    validate_user_ongoing_emissions(e, state);
    storage::set_user_ongoing_emissions(e, tier, user, pool, state);
}

fn accrue_user_ongoing_emissions(
    e: &Env,
    mut state: UserOngoingEmissions,
    active_shares: i128,
    current_index: i128,
) -> UserOngoingEmissions {
    if active_shares < 0 || current_index < state.index {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let index_delta = checked_sub(e, current_index, state.index);
    let numerator = I256::from_i128(e, active_shares)
        .mul(&I256::from_i128(e, index_delta))
        .add(&I256::from_i128(e, state.carry));
    let scale = I256::from_i128(e, SCALAR_14);
    let accrued = numerator
        .div(&scale)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let carry = numerator
        .sub(&I256::from_i128(e, accrued).mul(&scale))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    state.accrued = checked_add(e, state.accrued, accrued);
    state.carry = carry;
    state.index = current_index;
    state
}

fn checkpoint_user_ongoing_emissions(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
) -> UserOngoingEmissions {
    require_emission_tier(e, tier);
    let mut pool_state = get_pool_ongoing_emissions(e, pool);
    let current_index = advance_pool_tier_stream(e, &mut pool_state, tier);
    set_pool_ongoing_emissions(e, pool, &pool_state);
    let state = accrue_user_ongoing_emissions(
        e,
        get_user_ongoing_emissions(e, tier, user, pool),
        storage::get_user_balance_for_tier(e, tier, pool, user).shares,
        current_index,
    );
    set_user_ongoing_emissions(e, tier, user, pool, &state);
    state
}

fn advance_pool_tier_stream(e: &Env, state: &mut PoolOngoingEmissions, tier: BackstopTier) -> i128 {
    let now = e.ledger().timestamp();
    match tier {
        BackstopTier::BlndUsdc => {
            advance_tier_stream(
                e,
                &mut state.blnd_usdc_stream,
                state.active_blnd_usdc_shares,
                now,
            );
            state.blnd_usdc_stream.index
        }
        BackstopTier::BlndXlm => {
            advance_tier_stream(
                e,
                &mut state.blnd_xlm_stream,
                state.active_blnd_xlm_shares,
                now,
            );
            state.blnd_xlm_stream.index
        }
        BackstopTier::Usdc => panic_with_error!(e, BackstopError::InvalidEmissionValue),
    }
}

fn advance_tier_stream(e: &Env, stream: &mut TierEmissionStream, active_shares: i128, now: u64) {
    validate_tier_stream(e, stream);
    if active_shares < 0 || stream.last_time > now {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let stream_end = now.min(stream.expiration);
    if active_shares > 0 && stream_end > stream.last_time {
        let elapsed = stream_end - stream.last_time;
        let emitted_scaled = I256::from_i128(e, i128::from(elapsed))
            .mul(&I256::from_i128(e, i128::from(stream.eps)))
            .add(&if stream_end == stream.expiration {
                I256::from_i128(e, stream.schedule_carry)
            } else {
                I256::from_i128(e, 0)
            });
        let numerator = emitted_scaled
            .mul(&I256::from_i128(e, SCALAR_7))
            .add(&I256::from_i128(e, stream.index_carry));
        let denominator = I256::from_i128(e, active_shares);
        let index_increment = numerator
            .div(&denominator)
            .to_i128()
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        stream.index_carry = numerator
            .sub(&I256::from_i128(e, index_increment).mul(&denominator))
            .to_i128()
            .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
        stream.index = checked_add(e, stream.index, index_increment);
    }
    if stream_end == stream.expiration && stream_end > stream.last_time {
        stream.schedule_carry = 0;
    }
    stream.last_time = now;
}

fn refresh_tier_stream(
    e: &Env,
    stream: &mut TierEmissionStream,
    active_shares: i128,
    pending: i128,
    now: u64,
) {
    if pending < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    advance_tier_stream(e, stream, active_shares, now);
    if pending == 0 {
        return;
    }

    let remaining_seconds = stream.expiration.saturating_sub(now);
    let scaled_total = I256::from_i128(e, pending)
        .mul(&I256::from_i128(e, SCALAR_7))
        .add(
            &I256::from_i128(e, i128::from(remaining_seconds))
                .mul(&I256::from_i128(e, i128::from(stream.eps))),
        )
        .add(&I256::from_i128(e, stream.schedule_carry));
    let duration = I256::from_i128(e, i128::from(BACKSTOP_EMISSION_STREAM_SECONDS));
    let eps = scaled_total
        .div(&duration)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    stream.schedule_carry = scaled_total
        .sub(&I256::from_i128(e, eps).mul(&duration))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    stream.eps =
        u64::try_from(eps).unwrap_or_else(|_| panic_with_error!(e, BackstopError::OverflowError));
    stream.expiration = now
        .checked_add(BACKSTOP_EMISSION_STREAM_SECONDS)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    stream.last_time = now;
}

fn allocate_pool_backstop_emissions(
    e: &Env,
    state: &mut PoolOngoingEmissions,
    values: &BlndEmissionValues,
    allocation: i128,
) {
    if allocation < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let total_weight = checked_add(e, values.blnd_usdc, values.blnd_xlm);
    let distribution = checked_add(e, allocation, state.backstop_tier_carry);
    if total_weight == 0 {
        return;
    }

    let blnd_usdc = proportional_floor(e, distribution, values.blnd_usdc, total_weight);
    let blnd_xlm = proportional_floor(e, distribution, values.blnd_xlm, total_weight);
    state.backstop_tier_carry = checked_sub(e, distribution, checked_add(e, blnd_usdc, blnd_xlm));
    state.pending_blnd_usdc = checked_add(e, state.pending_blnd_usdc, blnd_usdc);
    state.pending_blnd_xlm = checked_add(e, state.pending_blnd_xlm, blnd_xlm);
}

fn set_ongoing_emission_state(e: &Env, state: &OngoingEmissionState) {
    validate_ongoing_emission_state(e, state);
    let accounted = checked_add(
        e,
        checked_add(e, state.backstop_allocated, state.pool_allocated),
        checked_add(
            e,
            state.split_carry,
            checked_add(e, state.backstop_carry, state.pool_carry),
        ),
    );
    if accounted != state.total_distributed {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    storage::set_ongoing_emission_state(e, state);
}

fn set_pool_ongoing_emissions(e: &Env, pool: &Address, state: &PoolOngoingEmissions) {
    validate_pool_ongoing_emissions(e, state);
    storage::set_pool_ongoing_emissions(e, pool, state);
}

fn validate_ongoing_emission_state(e: &Env, state: &OngoingEmissionState) {
    if state.backstop_allocated < 0
        || state.backstop_carry < 0
        || state.backstop_claimed < 0
        || state.pool_allocated < 0
        || state.pool_carry < 0
        || state.split_carry < 0
        || state.total_distributed < 0
        || state.backstop_claimed > state.backstop_allocated
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn validate_pool_ongoing_emissions(e: &Env, state: &PoolOngoingEmissions) {
    if state.accrued_backstop < 0
        || state.accrued_pool < 0
        || state.active_blnd_usdc < 0
        || state.active_blnd_usdc_shares < 0
        || state.active_blnd_xlm < 0
        || state.active_blnd_xlm_shares < 0
        || state.backstop_tier_carry < 0
        || state.pending_blnd_usdc < 0
        || state.pending_blnd_xlm < 0
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    validate_tier_stream(e, &state.blnd_usdc_stream);
    validate_tier_stream(e, &state.blnd_xlm_stream);
}

fn validate_tier_stream(e: &Env, stream: &TierEmissionStream) {
    if stream.index < 0
        || stream.index_carry < 0
        || stream.schedule_carry < 0
        || stream.schedule_carry >= i128::from(BACKSTOP_EMISSION_STREAM_SECONDS)
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn validate_user_ongoing_emissions(e: &Env, state: &UserOngoingEmissions) {
    if state.accrued < 0 || state.carry < 0 || state.carry >= SCALAR_14 || state.index < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .filter(|result| *result >= 0)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidOngoingBalance))
}

fn checked_signed_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn emissions_for_seconds(e: &Env, seconds: u64) -> i128 {
    i128::from(seconds)
        .checked_mul(SCALAR_7)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
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
        migration, storage,
        testutils::{
            create_backstop, create_blnd_token, create_comet_lp_pool, create_emitter,
            create_mock_pool_factory, create_token, create_usdc_token,
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

        fn pool_emissions(&self, pool: &Address) -> PoolOngoingEmissions {
            self.e
                .as_contract(&self.backstop, || get_pool_ongoing_emissions(&self.e, pool))
        }

        fn ongoing_state(&self) -> OngoingEmissionState {
            self.e
                .as_contract(&self.backstop, || get_ongoing_emission_state(&self.e))
        }

        fn distribution(&self) -> OngoingDistribution {
            self.e
                .as_contract(&self.backstop, || super::distribute(&self.e))
        }

        fn pool(&self, blnd_usdc: i128, blnd_xlm: i128) -> Address {
            let pool = Address::generate(&self.e);
            MockPoolFactoryClient::new(&self.e, &self.factory).set_mock_pool(&pool);
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

    fn empty_stream() -> TierEmissionStream {
        TierEmissionStream {
            eps: 0,
            expiration: 0,
            index: 0,
            index_carry: 0,
            last_time: 0,
            schedule_carry: 0,
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
                    10 * SCALAR_7,
                    BlndEmissionValues {
                        blnd_usdc: 10 * SCALAR_7,
                        blnd_xlm: 0,
                    },
                ),
                (
                    second,
                    20 * SCALAR_7,
                    BlndEmissionValues {
                        blnd_usdc: 20 * SCALAR_7,
                        blnd_xlm: 0,
                    },
                ),
            ]
        );
    }

    #[test]
    fn tier_stream_rolls_unfinished_emissions_into_a_fresh_seven_days() {
        let e = Env::default();
        let active_shares = 10 * SCALAR_7;
        let allocation = 7 * SCALAR_7;
        let start = 1_000;
        let mut stream = empty_stream();

        refresh_tier_stream(&e, &mut stream, active_shares, allocation, start);
        assert_eq!(stream.expiration, start + BACKSTOP_EMISSION_STREAM_SECONDS);

        let next_gulp = start + 24 * 60 * 60;
        advance_tier_stream(&e, &mut stream, active_shares, next_gulp);
        let first_day = accrue_user_ongoing_emissions(
            &e,
            UserOngoingEmissions {
                accrued: 0,
                carry: 0,
                index: 0,
            },
            active_shares,
            stream.index,
        );
        assert!((SCALAR_7 - 1..=SCALAR_7).contains(&first_day.accrued));

        refresh_tier_stream(&e, &mut stream, active_shares, allocation, next_gulp);
        assert_eq!(
            stream.expiration,
            next_gulp + BACKSTOP_EMISSION_STREAM_SECONDS
        );
        advance_tier_stream(
            &e,
            &mut stream,
            active_shares,
            next_gulp + BACKSTOP_EMISSION_STREAM_SECONDS,
        );
        let completed = accrue_user_ongoing_emissions(
            &e,
            UserOngoingEmissions {
                accrued: 0,
                carry: 0,
                index: 0,
            },
            active_shares,
            stream.index,
        );
        assert_eq!(completed.accrued, 2 * allocation);
        assert_eq!(stream.schedule_carry, 0);
    }

    #[test]
    fn allocates_by_emitter_checkpoint_and_ignores_unrelated_blnd() {
        let fixture = Fixture::create();
        let first = fixture.pool(10 * SCALAR_7, 0);
        let second = fixture.pool(0, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, first.clone(), second.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        assert_eq!(
            fixture.distribution(),
            OngoingDistribution {
                backstop_allocated: 7 * SCALAR_7,
                backstop_carry: 0,
                checkpoint: 1_010,
                eligible_blnd: 200 * SCALAR_7,
                pool_allocated: 3 * SCALAR_7,
                pool_carry: 0,
                distributed: 10 * SCALAR_7,
                split_carry: 0,
            }
        );
        for pool in [first.clone(), second.clone()] {
            assert_eq!(
                fixture.pool_emissions(&pool),
                PoolOngoingEmissions {
                    accrued_backstop: 35_000_000,
                    accrued_pool: 15_000_000,
                    active_blnd_usdc: if pool == first { 10 * SCALAR_7 } else { 0 },
                    active_blnd_usdc_shares: if pool == first { 10 * SCALAR_7 } else { 0 },
                    active_blnd_xlm: if pool == second { 10 * SCALAR_7 } else { 0 },
                    active_blnd_xlm_shares: if pool == second { 10 * SCALAR_7 } else { 0 },
                    backstop_tier_carry: 0,
                    blnd_usdc_stream: empty_stream(),
                    blnd_xlm_stream: empty_stream(),
                    pending_blnd_usdc: if pool == first { 35_000_000 } else { 0 },
                    pending_blnd_xlm: if pool == second { 35_000_000 } else { 0 },
                }
            );
        }
        assert!(fixture.client().migration_state().blnd_binding_verified);

        MockTokenClient::new(&fixture.e, &fixture.blnd).mint(&fixture.backstop, &1);
        fixture.e.ledger().set_timestamp(1_015);
        let second_distribution = fixture.distribution();
        assert_eq!(second_distribution.distributed, 5 * SCALAR_7);
        assert_eq!(second_distribution.split_carry, 0);
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

        assert!(!fixture.client().migration_state().blnd_binding_verified);
        assert_eq!(fixture.client().distribute(), 5 * SCALAR_7);
        assert!(fixture.client().migration_state().blnd_binding_verified);
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
        assert!(fixture.client().migration_state().blnd_binding_verified);

        fixture.e.ledger().set_timestamp(1_015);
        assert_eq!(fixture.client().distribute(), 5 * SCALAR_7);
        assert_eq!(fixture.ongoing_state().total_distributed, 15 * SCALAR_7);
    }

    #[test]
    fn claimable_is_read_only_and_validates_scope() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        fixture.user_position(BackstopTier::BlndUsdc, &user, &pool, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);
        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(
            fixture.client().claimable(
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
            .set_timestamp(1_010 + BACKSTOP_EMISSION_STREAM_SECONDS);

        let stored_before = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_user_ongoing_emissions(&fixture.e, BackstopTier::BlndUsdc, &user, &pool)
        });
        assert_eq!(
            fixture.client().claimable(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone()],
            ),
            7 * SCALAR_7
        );
        assert!(fixture.e.auths().is_empty());
        let stored_after = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_user_ongoing_emissions(&fixture.e, BackstopTier::BlndUsdc, &user, &pool)
        });
        assert_eq!(stored_after, stored_before);
        assert!(fixture
            .client()
            .try_claimable(&BackstopTier::Usdc, &user, &vec![&fixture.e, pool.clone()],)
            .is_err());
        assert!(fixture
            .client()
            .try_claimable(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, Address::generate(&fixture.e)],
            )
            .is_err());
        assert!(fixture
            .client()
            .try_claimable(&BackstopTier::BlndUsdc, &user, &vec![&fixture.e])
            .is_err());
        assert!(fixture
            .client()
            .try_claimable(
                &BackstopTier::BlndUsdc,
                &user,
                &vec![&fixture.e, pool.clone(), pool],
            )
            .is_err());
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
            .set_timestamp(1_010 + BACKSTOP_EMISSION_STREAM_SECONDS);
        assert_eq!(
            fixture.client().claimable(
                &BackstopTier::BlndUsdc,
                &blnd_usdc_user,
                &vec![&fixture.e, pool.clone()],
            ),
            35_000_000
        );
        assert_eq!(
            fixture.client().claimable(
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
            fixture.client().claimable(
                &BackstopTier::BlndUsdc,
                &blnd_usdc_user,
                &vec![&fixture.e, pool.clone()],
            ),
            0
        );
        assert_eq!(
            fixture.client().claimable(
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
            .set_timestamp(1_010 + BACKSTOP_EMISSION_STREAM_SECONDS);
        fixture.client().distribute();
        let accrued = fixture.client().claimable(
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
            fixture.client().claimable(
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
            .set_timestamp(1_010 + BACKSTOP_EMISSION_STREAM_SECONDS);
        fixture.client().distribute();

        let pool_addresses = vec![&fixture.e, first.clone(), second.clone()];
        let aggregate_claim =
            fixture
                .client()
                .claimable(&BackstopTier::BlndUsdc, &user, &pool_addresses);
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
            fixture
                .client()
                .claimable(&BackstopTier::BlndUsdc, &user, &pool_addresses,),
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
    fn claim_rejects_empty_and_duplicate_pool_addresses() {
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
        assert!(!fixture.client().migration_state().blnd_binding_verified);
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

        let distribution = fixture.distribution();
        assert_eq!(distribution.distributed, 5 * SCALAR_7);
        assert_eq!(distribution.eligible_blnd, 300 * SCALAR_7);
        for pool in pools.iter() {
            let emissions = fixture.pool_emissions(&pool);
            assert!(emissions.accrued_backstop > 0);
            assert!(emissions.accrued_pool > 0);
        }
    }

    #[test]
    fn zero_eligible_blnd_is_carried_until_a_blnd_tier_is_deposited() {
        let fixture = Fixture::create();
        let pool = fixture.pool(0, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);
        fixture.e.ledger().set_timestamp(1_005);

        assert_eq!(
            fixture.distribution(),
            OngoingDistribution {
                backstop_allocated: 0,
                backstop_carry: 35_000_000,
                checkpoint: 1_005,
                eligible_blnd: 0,
                pool_allocated: 0,
                pool_carry: 15_000_000,
                distributed: 5 * SCALAR_7,
                split_carry: 0,
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
        assert_eq!(
            fixture.distribution(),
            OngoingDistribution {
                backstop_allocated: 7 * SCALAR_7,
                backstop_carry: 0,
                checkpoint: 1_010,
                eligible_blnd: 10 * SCALAR_7,
                pool_allocated: 3 * SCALAR_7,
                pool_carry: 0,
                distributed: 5 * SCALAR_7,
                split_carry: 0,
            }
        );
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
