use sep_41_token::TokenClient;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contracttype, panic_with_error, vec, Address, Env, IntoVal, Map, Symbol, Val, Vec,
};

#[cfg(test)]
use crate::storage::UserEmissionData;
use crate::{
    backstop::{
        credit_tier_shares, require_registered_pool, tier_token, BackstopTier, BlndEmissionValues,
    },
    constants::{MAX_BACKFILLED_EMISSIONS, SCALAR_7},
    dependencies::{CometClient, EmitterClient},
    errors::BackstopError,
    migration,
    storage::{self, OngoingEmissionState, PoolOngoingEmissions},
};

use super::distributor;
use super::policy::{
    comet_composition, pool_active_emission_assets, proportional_floor, quote_ongoing_blnd_split,
    underlying_blnd_from_composition,
};

const MIN_DISTRIBUTION_INTERVAL_SECONDS: u64 = 5;
const WEIGHT_CHANGE_CHECKPOINT_MAX_AGE_SECONDS: u64 = 5;
const POOL_EMISSION_GULP_INTERVAL_SECONDS: u64 = 24 * 60 * 60;

/// Result of one completed permissionless BLND distribution checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype(export = false)]
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
    state.active_blnd_usdc = pool_active_emission_assets(e, BackstopTier::BlndUsdc, pool);
    state.active_blnd_xlm = pool_active_emission_assets(e, BackstopTier::BlndXlm, pool);
    set_pool_ongoing_emissions(e, pool, &state);
}

#[cfg(test)]
pub(crate) fn preview_user_ongoing_emissions(
    e: &Env,
    tier: BackstopTier,
    user: &Address,
    pool: &Address,
) -> UserEmissionData {
    require_emission_tier(e, tier);
    distributor::preview_user_emissions(e, tier, pool, user)
}

#[cfg(test)]
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
        distributor::checkpoint_user_emissions(e, tier, pool, user);
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

        let pool_claim = distributor::claim_emissions(e, tier, &pool, from);
        claims.set(pool.clone(), pool_claim);
        blnd_amount = checked_add(e, blnd_amount, pool_claim);
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
    distributor::set_emission_eps(
        e,
        BackstopTier::BlndUsdc,
        pool,
        pool_state.pending_blnd_usdc,
    );
    distributor::set_emission_eps(e, BackstopTier::BlndXlm, pool, pool_state.pending_blnd_xlm);
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
    // Keep the cached active LP amount synchronized for the next global
    // allocation checkpoint. Tier stream indexes read canonical balances.
    if tier != BackstopTier::Usdc {
        refresh_pool_ongoing_assets(e, pool);
    }
}

fn require_emission_tier(e: &Env, tier: BackstopTier) {
    if tier == BackstopTier::Usdc {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
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
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let user = Address::generate(&fixture.e);
        let active_shares = 10 * SCALAR_7;
        let allocation = 7 * SCALAR_7;
        let start = 1_000;
        fixture.user_position(BackstopTier::BlndUsdc, &user, &pool, active_shares);
        fixture.e.as_contract(&fixture.backstop, || {
            distributor::set_emission_eps(&fixture.e, BackstopTier::BlndUsdc, &pool, allocation);
        });
        let stream = fixture.e.as_contract(&fixture.backstop, || {
            storage::get_backstop_emis_data(&fixture.e, BackstopTier::BlndUsdc, &pool).unwrap()
        });
        assert_eq!(stream.expiration, start + distributor::STREAM_SECONDS);

        let next_gulp = start + 24 * 60 * 60;
        fixture.e.ledger().set_timestamp(next_gulp);
        let first_day = fixture.e.as_contract(&fixture.backstop, || {
            distributor::checkpoint_user_emissions(&fixture.e, BackstopTier::BlndUsdc, &pool, &user)
        });
        assert!((SCALAR_7 - 1..=SCALAR_7).contains(&first_day.accrued));

        fixture.e.as_contract(&fixture.backstop, || {
            distributor::set_emission_eps(&fixture.e, BackstopTier::BlndUsdc, &pool, allocation);
        });
        fixture
            .e
            .ledger()
            .set_timestamp(next_gulp + distributor::STREAM_SECONDS);
        let completed = fixture.e.as_contract(&fixture.backstop, || {
            distributor::checkpoint_user_emissions(&fixture.e, BackstopTier::BlndUsdc, &pool, &user)
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

        let distribution = fixture.distribution();
        assert_eq!(distribution.distributed, 5 * SCALAR_7);
        assert_eq!(distribution.eligible_blnd, 300 * SCALAR_7);
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
