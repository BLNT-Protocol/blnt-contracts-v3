use crate::{
    backstop::{is_blnd_emission_tier, tier_for_token, BackstopTier},
    constants::{MAX_BACKFILLED_EMISSIONS, SCALAR_7},
    errors::BackstopError,
    migration,
    storage::{self, OngoingEmissionState, PoolOngoingEmissions},
};
use soroban_sdk::{panic_with_error, Address, Env, Vec};

use super::{
    distributor,
    policy::{
        comet_composition, pool_active_emission_assets, pool_spot_blnd_emission_weight,
        proportional_floor, quote_ongoing_blnd_split, underlying_blnd_from_composition,
    },
};

pub(crate) fn pool_weight(e: &Env, pool: &Address) -> i128 {
    if migration::is_active(e) {
        pool_spot_blnd_emission_weight(e, pool)
    } else {
        let token = storage::get_blnd_usdc_token(e);
        tier_for_token(e, pool, &token)
            .map(|tier| pool_active_emission_assets(e, tier, pool))
            .unwrap_or(0)
    }
}

/// Accrue migration backfill through the ordinary 70/30 emission pipeline.
/// Before activation, active BLND:USDC LP tokens directly determine pool
/// weight, and only that tier receives pending backstop emissions.
pub(crate) fn checkpoint_backfill(e: &Env, checkpoint: u64) -> i128 {
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

pub(crate) fn collect_weights(
    e: &Env,
    use_underlying_blnd: bool,
) -> (Vec<(Address, PoolOngoingEmissions, i128, i128)>, i128) {
    let reward_zone = storage::get_reward_zone(e);
    if reward_zone.is_empty() {
        return (Vec::new(e), 0);
    }
    let (blnd_usdc_supply, blnd_usdc_reserve) = if use_underlying_blnd {
        comet_composition(e, &storage::get_blnd_usdc_token(e))
    } else {
        (0, 0)
    };
    let (blnd_xlm_supply, blnd_xlm_reserve) = if use_underlying_blnd {
        comet_composition(e, &storage::get_blnd_xlm_token(e))
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
        weights.push_back((pool, pool_state, pool_blnd_usdc, pool_blnd_xlm));
    }
    if use_underlying_blnd
        && (total_blnd_usdc > blnd_usdc_supply || total_blnd_xlm > blnd_xlm_supply)
    {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }
    (weights, total_eligible_blnd)
}

pub(crate) fn allocate_distribution(
    e: &Env,
    state: &mut OngoingEmissionState,
    amount: i128,
    checkpoint: u64,
    weights: Vec<(Address, PoolOngoingEmissions, i128, i128)>,
    total_eligible_blnd: i128,
) -> i128 {
    let (backstop_split, pool_split, split_carry) =
        quote_ongoing_blnd_split(e, amount, state.split_carry);
    let backstop_distribution = checked_add(e, backstop_split, state.backstop_carry);
    let pool_distribution = checked_add(e, pool_split, state.pool_carry);
    let mut backstop_allocated = 0_i128;
    let mut pool_allocated = 0_i128;
    for (pool, mut pool_state, blnd_usdc, blnd_xlm) in weights.iter() {
        let weight = checked_add(e, blnd_usdc, blnd_xlm);
        let (backstop_allocation, pool_allocation) = if total_eligible_blnd == 0 {
            (0, 0)
        } else {
            (
                proportional_floor(e, backstop_distribution, weight, total_eligible_blnd),
                proportional_floor(e, pool_distribution, weight, total_eligible_blnd),
            )
        };
        allocate_pool_backstop_emissions(
            e,
            &mut pool_state,
            blnd_usdc,
            blnd_xlm,
            backstop_allocation,
        );
        pool_state.accrued_pool = checked_add(e, pool_state.accrued_pool, pool_allocation);
        set_pool_ongoing_emissions(e, &pool, &pool_state);
        backstop_allocated = checked_add(e, backstop_allocated, backstop_allocation);
        pool_allocated = checked_add(e, pool_allocated, pool_allocation);
    }

    state.total_distributed = checked_add(e, state.total_distributed, amount);
    state.backstop_allocated = checked_add(e, state.backstop_allocated, backstop_allocated);
    state.pool_allocated = checked_add(e, state.pool_allocated, pool_allocated);
    state.split_carry = split_carry;
    state.backstop_carry = checked_sub(e, backstop_distribution, backstop_allocated);
    state.pool_carry = checked_sub(e, pool_distribution, pool_allocated);
    state.last_distribution = Some(checkpoint);
    set_ongoing_emission_state(e, state);
    storage::set_reward_zone_checkpoint(e, checkpoint);
    storage::set_reward_zone_distribution_started(e);

    amount
}

pub(crate) fn advance_without_distribution(
    e: &Env,
    state: &mut OngoingEmissionState,
    checkpoint: u64,
) -> i128 {
    state.last_distribution = Some(checkpoint);
    set_ongoing_emission_state(e, state);
    storage::set_reward_zone_checkpoint(e, checkpoint);
    storage::set_reward_zone_distribution_started(e);
    0
}

pub(crate) fn get_ongoing_emission_state(e: &Env) -> OngoingEmissionState {
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
    state.active_blnd_usdc = tier_for_token(e, pool, &storage::get_blnd_usdc_token(e))
        .map(|tier| pool_active_emission_assets(e, tier, pool))
        .unwrap_or(0);
    state.active_blnd_xlm = tier_for_token(e, pool, &storage::get_blnd_xlm_token(e))
        .map(|tier| pool_active_emission_assets(e, tier, pool))
        .unwrap_or(0);
    set_pool_ongoing_emissions(e, pool, &state);
}

pub(crate) fn checkpoint_user_ongoing_for_weight_change(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
    emission_eligible: bool,
) {
    if emission_eligible {
        distributor::update_emissions(e, tier, pool, user);
    }
}

/// Preserve the migration transition gate while inheriting v2's global
/// weight-sampling behavior. Tier and user streams are checkpointed
/// separately before their balances change.
pub(crate) fn prepare_pool_weight_change(e: &Env, tier: BackstopTier, pool: &Address) -> bool {
    let emission_eligible = is_blnd_emission_tier(e, pool, tier);
    if emission_eligible {
        migration::require_weight_mutation_allowed(e);
    }
    emission_eligible
}

pub(crate) fn finish_pool_weight_change(e: &Env, pool: &Address, emission_eligible: bool) {
    // Keep the cached active LP amount synchronized for the next global
    // allocation checkpoint. Tier stream indexes read canonical balances.
    if emission_eligible {
        refresh_pool_ongoing_assets(e, pool);
    }
}

fn allocate_pool_backstop_emissions(
    e: &Env,
    state: &mut PoolOngoingEmissions,
    blnd_usdc_weight: i128,
    blnd_xlm_weight: i128,
    allocation: i128,
) {
    if allocation < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let total_weight = checked_add(e, blnd_usdc_weight, blnd_xlm_weight);
    let distribution = checked_add(e, allocation, state.backstop_tier_carry);
    if total_weight == 0 {
        return;
    }

    let blnd_usdc = proportional_floor(e, distribution, blnd_usdc_weight, total_weight);
    let blnd_xlm = proportional_floor(e, distribution, blnd_xlm_weight, total_weight);
    state.backstop_tier_carry = checked_sub(e, distribution, checked_add(e, blnd_usdc, blnd_xlm));
    state.pending_blnd_usdc = checked_add(e, state.pending_blnd_usdc, blnd_usdc);
    state.pending_blnd_xlm = checked_add(e, state.pending_blnd_xlm, blnd_xlm);
}

pub(crate) fn set_ongoing_emission_state(e: &Env, state: &OngoingEmissionState) {
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

pub(crate) fn set_pool_ongoing_emissions(e: &Env, pool: &Address, state: &PoolOngoingEmissions) {
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
    if state.accrued_pool < 0
        || state.active_blnd_usdc < 0
        || state.active_blnd_xlm < 0
        || state.backstop_tier_carry < 0
        || state.pending_blnd_usdc < 0
        || state.pending_blnd_xlm < 0
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

pub(crate) fn checked_add(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_add(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

fn checked_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .filter(|result| *result >= 0)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::InvalidOngoingBalance))
}

pub(crate) fn checked_signed_sub(e: &Env, left: i128, right: i128) -> i128 {
    left.checked_sub(right)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}

pub(crate) fn emissions_for_seconds(e: &Env, seconds: u64) -> i128 {
    i128::from(seconds)
        .checked_mul(SCALAR_7)
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError))
}
