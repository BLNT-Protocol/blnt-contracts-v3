use sep_41_token::TokenClient;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Vec, I256};

use crate::{
    backstop::{require_registered_pool, BackstopTier, BlndEmissionValues},
    constants::SCALAR_14,
    dependencies::EmitterClient,
    errors::BackstopError,
    migration,
    storage::{
        self, OngoingEmissionState, PoolEmissionReservation, PoolOngoingEmissions,
        UserOngoingEmissions,
    },
};

use super::policy::{
    comet_composition, pool_active_emission_assets, proportional_floor, quote_ongoing_blnd_split,
    underlying_blnd_from_composition,
};

const MIN_DISTRIBUTION_INTERVAL_SECONDS: u64 = 5;
const WEIGHT_CHANGE_CHECKPOINT_MAX_AGE_SECONDS: u64 = 5;
const POOL_EMISSION_GULP_INTERVAL_SECONDS: u64 = 24 * 60 * 60;

/// Result of one completed permissionless ongoing-emission distribution.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct OngoingDistribution {
    pub backstop_allocated: i128,
    pub backstop_carry: i128,
    pub checkpoint: u64,
    pub eligible_blnd: i128,
    pub pool_allocated: i128,
    pub pool_carry: i128,
    pub received: i128,
    pub split_carry: i128,
}

pub(crate) fn distribute(e: &Env) -> OngoingDistribution {
    migration::require_active(e);
    let backstop = e.current_contract_address();
    let emitter = EmitterClient::new(e, &storage::get_emitter(e));
    if emitter.get_backstop() != backstop {
        panic_with_error!(e, BackstopError::EmitterDidNotMigrate);
    }

    let reward_zone = storage::get_reward_zone(e);
    let mut state = get_ongoing_emission_state(e);
    if reward_zone.is_empty() {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }

    let (blnd_usdc_supply, blnd_usdc_reserve) = comet_composition(e, BackstopTier::BlndUsdc);
    let (blnd_xlm_supply, blnd_xlm_reserve) = comet_composition(e, BackstopTier::BlndXlm);
    let mut weights: Vec<(Address, i128, BlndEmissionValues)> = Vec::new(e);
    let mut total_eligible_blnd = 0_i128;
    let mut total_blnd_usdc = 0_i128;
    let mut total_blnd_xlm = 0_i128;
    for pool in reward_zone.iter() {
        let pool_state = get_pool_ongoing_emissions(e, &pool);
        total_blnd_usdc = checked_add(e, total_blnd_usdc, pool_state.active_blnd_usdc);
        total_blnd_xlm = checked_add(e, total_blnd_xlm, pool_state.active_blnd_xlm);
        let pool_blnd_usdc = underlying_blnd_from_composition(
            e,
            pool_state.active_blnd_usdc,
            blnd_usdc_supply,
            blnd_usdc_reserve,
        );
        let pool_blnd_xlm = underlying_blnd_from_composition(
            e,
            pool_state.active_blnd_xlm,
            blnd_xlm_supply,
            blnd_xlm_reserve,
        );
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
    if total_blnd_usdc > blnd_usdc_supply || total_blnd_xlm > blnd_xlm_supply {
        panic_with_error!(e, BackstopError::NoEligibleWeight);
    }

    let last_distribution = state.last_distribution.unwrap();
    let blnd = TokenClient::new(e, &storage::get_blnd_token(e));
    let binding_verified = storage::get_blnd_binding_verified(e);
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
    if elapsed < MIN_DISTRIBUTION_INTERVAL_SECONDS || checkpoint > e.ledger().timestamp() {
        panic_with_error!(e, BackstopError::DistributionTooSoon);
    }

    let outstanding = checked_sub(e, state.total_received, state.total_claimed);
    let protected_balance = checked_add(e, storage::get_remaining_backfill_reserve(e), outstanding);
    let balance = blnd.balance(&backstop);
    if let Some(balance_before) = balance_before {
        let binding_delta = checked_signed_sub(e, balance, balance_before);
        if emitted <= 0 || binding_delta != emitted {
            panic_with_error!(e, BackstopError::InvalidOngoingBalance);
        }
    }
    if balance < protected_balance {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    let received = checked_sub(e, balance, protected_balance);
    if received <= 0 || received < emitted {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }

    let split = quote_ongoing_blnd_split(e, received, state.split_carry);
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

    state.total_received = checked_add(e, state.total_received, received);
    state.backstop_allocated = checked_add(e, state.backstop_allocated, backstop_allocated);
    state.pool_allocated = checked_add(e, state.pool_allocated, pool_allocated);
    state.split_carry = split.carry;
    state.backstop_carry = checked_sub(e, backstop_distribution, backstop_allocated);
    state.pool_carry = checked_sub(e, pool_distribution, pool_allocated);
    state.last_distribution = Some(checkpoint);
    set_ongoing_emission_state(e, &state);
    if !binding_verified {
        storage::set_blnd_binding_verified(e);
    }
    storage::set_reward_zone_checkpoint(e, checkpoint);

    OngoingDistribution {
        backstop_allocated,
        backstop_carry: state.backstop_carry,
        checkpoint,
        eligible_blnd: total_eligible_blnd,
        pool_allocated,
        pool_carry: state.pool_carry,
        received,
        split_carry: state.split_carry,
    }
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
    let pool_state = get_pool_ongoing_emissions(e, pool);
    accrue_user_ongoing_emissions(
        e,
        get_user_ongoing_emissions(e, tier, user, pool),
        storage::get_user_balance_for_tier(e, tier, pool, user).shares,
        pool_ongoing_index(e, &pool_state, tier),
    )
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
    user: &Address,
    pool: &Address,
    recipient: &Address,
) -> i128 {
    user.require_auth();
    require_registered_pool(e, pool);

    let mut blnd_usdc = checkpoint_user_ongoing_emissions(e, BackstopTier::BlndUsdc, user, pool);
    let mut blnd_xlm = checkpoint_user_ongoing_emissions(e, BackstopTier::BlndXlm, user, pool);
    let amount = checked_add(e, blnd_usdc.accrued, blnd_xlm.accrued);
    if amount <= 0 {
        panic_with_error!(e, BackstopError::NoOngoingEmissions);
    }

    blnd_usdc.accrued = 0;
    blnd_xlm.accrued = 0;
    set_user_ongoing_emissions(e, BackstopTier::BlndUsdc, user, pool, &blnd_usdc);
    set_user_ongoing_emissions(e, BackstopTier::BlndXlm, user, pool, &blnd_xlm);

    let mut pool_state = get_pool_ongoing_emissions(e, pool);
    pool_state.accrued_backstop = checked_sub(e, pool_state.accrued_backstop, amount);
    set_pool_ongoing_emissions(e, pool, &pool_state);

    let mut ongoing = get_ongoing_emission_state(e);
    ongoing.backstop_claimed = checked_add(e, ongoing.backstop_claimed, amount);
    ongoing.total_claimed = checked_add(e, ongoing.total_claimed, amount);
    set_ongoing_emission_state(e, &ongoing);

    TokenClient::new(e, &storage::get_blnd_token(e)).transfer(
        &e.current_contract_address(),
        recipient,
        &amount,
    );
    amount
}

pub(crate) fn get_pool_emission_reservation(e: &Env, pool: &Address) -> PoolEmissionReservation {
    let reservation = storage::get_pool_emission_reservation(e, pool);
    validate_pool_emission_reservation(e, &reservation);
    reservation
}

pub(crate) fn gulp_pool_ongoing_emissions(e: &Env, pool: &Address) -> i128 {
    pool.require_auth();
    require_registered_pool(e, pool);

    let mut reservation = get_pool_emission_reservation(e, pool);
    let now = e.ledger().timestamp();
    if reservation.last_gulp.is_some_and(|last_gulp| {
        last_gulp
            .checked_add(POOL_EMISSION_GULP_INTERVAL_SECONDS)
            .is_none_or(|next_gulp| next_gulp > now)
    }) {
        panic_with_error!(e, BackstopError::PoolEmissionGulpTooSoon);
    }

    let mut pool_state = get_pool_ongoing_emissions(e, pool);
    let amount = pool_state.accrued_pool;
    if amount == 0 {
        return 0;
    }
    pool_state.accrued_pool = 0;
    reservation.available = checked_add(e, reservation.available, amount);
    reservation.last_gulp = Some(now);
    set_pool_ongoing_emissions(e, pool, &pool_state);
    set_pool_emission_reservation(e, pool, &reservation);
    amount
}

pub(crate) fn claim_reserved_pool_emissions(
    e: &Env,
    pool: &Address,
    recipient: &Address,
    amount: i128,
) {
    if amount <= 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    pool.require_auth();
    require_registered_pool(e, pool);

    let mut reservation = get_pool_emission_reservation(e, pool);
    reservation.available = checked_sub(e, reservation.available, amount);
    set_pool_emission_reservation(e, pool, &reservation);

    let mut ongoing = get_ongoing_emission_state(e);
    ongoing.total_claimed = checked_add(e, ongoing.total_claimed, amount);
    set_ongoing_emission_state(e, &ongoing);

    TokenClient::new(e, &storage::get_blnd_token(e)).transfer(
        &e.current_contract_address(),
        recipient,
        &amount,
    );
}

pub(crate) fn prepare_pool_weight_change(e: &Env, tier: BackstopTier, pool: &Address) {
    if tier == BackstopTier::Usdc {
        return;
    }
    migration::require_weight_mutation_allowed(e);
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
    if tier != BackstopTier::Usdc && storage::get_reward_zone(e).contains(pool.clone()) {
        refresh_pool_ongoing_assets(e, pool);
    }
}

fn require_emission_tier(e: &Env, tier: BackstopTier) {
    if tier == BackstopTier::Usdc {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn pool_ongoing_index(e: &Env, state: &PoolOngoingEmissions, tier: BackstopTier) -> i128 {
    match tier {
        BackstopTier::BlndUsdc => state.blnd_usdc_index,
        BackstopTier::BlndXlm => state.blnd_xlm_index,
        BackstopTier::Usdc => panic_with_error!(e, BackstopError::InvalidEmissionValue),
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
    let state = preview_user_ongoing_emissions(e, tier, user, pool);
    set_user_ongoing_emissions(e, tier, user, pool, &state);
    state
}

fn advance_ongoing_emission_index(
    e: &Env,
    allocation: i128,
    active_shares: i128,
    index: i128,
    carry: i128,
) -> (i128, i128) {
    if allocation < 0 || active_shares < 0 || index < 0 || carry < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    if active_shares == 0 {
        if allocation != 0 {
            panic_with_error!(e, BackstopError::InvalidEmissionValue);
        }
        return (index, carry);
    }

    let numerator = I256::from_i128(e, allocation)
        .mul(&I256::from_i128(e, SCALAR_14))
        .add(&I256::from_i128(e, carry));
    let denominator = I256::from_i128(e, active_shares);
    let index_increment = numerator
        .div(&denominator)
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    let next_carry = numerator
        .sub(&I256::from_i128(e, index_increment).mul(&denominator))
        .to_i128()
        .unwrap_or_else(|| panic_with_error!(e, BackstopError::OverflowError));
    (checked_add(e, index, index_increment), next_carry)
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
    (state.blnd_usdc_index, state.blnd_usdc_index_carry) = advance_ongoing_emission_index(
        e,
        blnd_usdc,
        state.active_blnd_usdc_shares,
        state.blnd_usdc_index,
        state.blnd_usdc_index_carry,
    );
    (state.blnd_xlm_index, state.blnd_xlm_index_carry) = advance_ongoing_emission_index(
        e,
        blnd_xlm,
        state.active_blnd_xlm_shares,
        state.blnd_xlm_index,
        state.blnd_xlm_index_carry,
    );
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
    if accounted != state.total_received {
        panic_with_error!(e, BackstopError::InvalidOngoingBalance);
    }
    storage::set_ongoing_emission_state(e, state);
}

fn set_pool_ongoing_emissions(e: &Env, pool: &Address, state: &PoolOngoingEmissions) {
    validate_pool_ongoing_emissions(e, state);
    storage::set_pool_ongoing_emissions(e, pool, state);
}

fn set_pool_emission_reservation(e: &Env, pool: &Address, reservation: &PoolEmissionReservation) {
    validate_pool_emission_reservation(e, reservation);
    storage::set_pool_emission_reservation(e, pool, reservation);
}

fn validate_ongoing_emission_state(e: &Env, state: &OngoingEmissionState) {
    if state.backstop_allocated < 0
        || state.backstop_carry < 0
        || state.backstop_claimed < 0
        || state.pool_allocated < 0
        || state.pool_carry < 0
        || state.split_carry < 0
        || state.total_claimed < 0
        || state.total_received < 0
        || state.backstop_claimed > state.backstop_allocated
        || state.backstop_claimed > state.total_claimed
        || state.total_claimed > state.total_received
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
    let pool_claimed = checked_sub(e, state.total_claimed, state.backstop_claimed);
    if pool_claimed > state.pool_allocated {
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
        || state.blnd_usdc_index < 0
        || state.blnd_usdc_index_carry < 0
        || state.blnd_xlm_index < 0
        || state.blnd_xlm_index_carry < 0
    {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn validate_user_ongoing_emissions(e: &Env, state: &UserOngoingEmissions) {
    if state.accrued < 0 || state.carry < 0 || state.carry >= SCALAR_14 || state.index < 0 {
        panic_with_error!(e, BackstopError::InvalidEmissionValue);
    }
}

fn validate_pool_emission_reservation(e: &Env, reservation: &PoolEmissionReservation) {
    if reservation.available < 0
        || reservation
            .last_gulp
            .is_some_and(|last_gulp| last_gulp > e.ledger().timestamp())
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

#[cfg(test)]
mod tests {
    use mock_pool_factory::MockPoolFactoryClient;
    use sep_41_token::{testutils::MockTokenClient, TokenClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        vec, Address, Env,
    };

    use crate::{
        backstop::{update_tier_totals, BackstopTier, PoolBalance, UserBalance},
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
                e,
                factory,
            }
        }

        fn client(&self) -> BackstopClient<'_> {
            BackstopClient::new(&self.e, &self.backstop)
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
                update_tier_totals(&self.e, BackstopTier::BlndUsdc, blnd_usdc, blnd_usdc, 0);
                update_tier_totals(&self.e, BackstopTier::BlndXlm, blnd_xlm, blnd_xlm, 0);
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
    fn reconciles_actual_blnd_and_conserves_all_carries() {
        let fixture = Fixture::create();
        let first = fixture.pool(10 * SCALAR_7, 0);
        let second = fixture.pool(0, 10 * SCALAR_7);
        fixture.set_reward_zone(&vec![&fixture.e, first.clone(), second.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        assert_eq!(
            fixture.client().distribute(),
            OngoingDistribution {
                backstop_allocated: 7 * SCALAR_7,
                backstop_carry: 0,
                checkpoint: 1_010,
                eligible_blnd: 200 * SCALAR_7,
                pool_allocated: 3 * SCALAR_7,
                pool_carry: 0,
                received: 10 * SCALAR_7,
                split_carry: 0,
            }
        );
        for pool in [first.clone(), second.clone()] {
            assert_eq!(
                fixture.client().pool_ongoing_emissions(&pool),
                PoolOngoingEmissions {
                    accrued_backstop: 35_000_000,
                    accrued_pool: 15_000_000,
                    active_blnd_usdc: if pool == first { 10 * SCALAR_7 } else { 0 },
                    active_blnd_usdc_shares: if pool == first { 10 * SCALAR_7 } else { 0 },
                    active_blnd_xlm: if pool == second { 10 * SCALAR_7 } else { 0 },
                    active_blnd_xlm_shares: if pool == second { 10 * SCALAR_7 } else { 0 },
                    backstop_tier_carry: 0,
                    blnd_usdc_index: if pool == first { 35_000_000_000_000 } else { 0 },
                    blnd_usdc_index_carry: 0,
                    blnd_xlm_index: if pool == second {
                        35_000_000_000_000
                    } else {
                        0
                    },
                    blnd_xlm_index_carry: 0,
                }
            );
        }
        assert!(fixture.client().blnd_binding_verified());

        MockTokenClient::new(&fixture.e, &fixture.blnd).mint(&fixture.backstop, &1);
        fixture.e.ledger().set_timestamp(1_015);
        let second_distribution = fixture.client().distribute();
        assert_eq!(second_distribution.received, 5 * SCALAR_7 + 1);
        assert_eq!(second_distribution.split_carry, 1);
        assert_eq!(
            fixture.client().ongoing_emission_state(),
            OngoingEmissionState {
                backstop_allocated: 105_000_000,
                backstop_carry: 0,
                backstop_claimed: 0,
                last_distribution: Some(1_015),
                pool_allocated: 45_000_000,
                pool_carry: 0,
                split_carry: 1,
                total_claimed: 0,
                total_received: 15 * SCALAR_7 + 1,
            }
        );
    }

    #[test]
    fn excludes_remaining_backfill_from_ongoing_receipts() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool]);

        MockTokenClient::new(&fixture.e, &fixture.blnd).mint(&fixture.backstop, &(5 * SCALAR_7));
        fixture.e.as_contract(&fixture.backstop, || {
            storage::set_backfill_funded_amount(&fixture.e, 5 * SCALAR_7);
        });

        fixture.e.ledger().set_timestamp(1_010);
        let distribution = fixture.client().distribute();
        assert_eq!(distribution.received, 10 * SCALAR_7);
        assert_eq!(
            fixture.client().ongoing_emission_state().total_received,
            10 * SCALAR_7
        );
        fixture.e.as_contract(&fixture.backstop, || {
            assert_eq!(
                storage::get_remaining_backfill_reserve(&fixture.e),
                5 * SCALAR_7
            );
            storage::set_total_backfill_claimed(&fixture.e, 2 * SCALAR_7);
            assert_eq!(
                storage::get_remaining_backfill_reserve(&fixture.e),
                3 * SCALAR_7
            );
        });
    }

    #[test]
    fn first_positive_distribution_binds_the_configured_blnd_token() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        fixture.set_reward_zone(&vec![&fixture.e, pool]);
        fixture.e.ledger().set_timestamp(1_005);

        assert!(!fixture.client().blnd_binding_verified());
        assert_eq!(fixture.client().distribute().received, 5 * SCALAR_7);
        assert!(fixture.client().blnd_binding_verified());
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&fixture.backstop),
            5 * SCALAR_7
        );
    }

    #[test]
    fn users_claim_only_the_two_blnd_tier_allocations() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 10 * SCALAR_7);
        let blnd_usdc_user = Address::generate(&fixture.e);
        let blnd_xlm_user = Address::generate(&fixture.e);
        let blnd_usdc_recipient = Address::generate(&fixture.e);
        let blnd_xlm_recipient = Address::generate(&fixture.e);
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
        assert_eq!(
            fixture
                .client()
                .user_ongoing_emissions(&blnd_usdc_user, &pool, &BackstopTier::BlndUsdc)
                .accrued,
            35_000_000
        );
        assert_eq!(
            fixture
                .client()
                .user_ongoing_emissions(&blnd_xlm_user, &pool, &BackstopTier::BlndXlm)
                .accrued,
            35_000_000
        );

        assert_eq!(
            fixture
                .client()
                .claim_ongoing_blnd(&blnd_usdc_user, &pool, &blnd_usdc_recipient),
            35_000_000
        );
        assert_eq!(
            fixture
                .client()
                .claim_ongoing_blnd(&blnd_xlm_user, &pool, &blnd_xlm_recipient),
            35_000_000
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&blnd_usdc_recipient),
            35_000_000
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&blnd_xlm_recipient),
            35_000_000
        );
        assert_eq!(
            fixture.client().ongoing_emission_state().backstop_claimed,
            7 * SCALAR_7
        );
        assert_eq!(
            fixture.client().pool_ongoing_emissions(&pool).accrued_pool,
            3 * SCALAR_7
        );
    }

    #[test]
    fn pool_tranche_is_reserved_and_paid_by_the_registered_pool() {
        let fixture = Fixture::create();
        let pool = fixture.pool(10 * SCALAR_7, 0);
        let recipient = Address::generate(&fixture.e);
        fixture.set_reward_zone(&vec![&fixture.e, pool.clone()]);

        fixture.e.ledger().set_timestamp(1_010);
        fixture.client().distribute();
        assert_eq!(fixture.client().gulp_pool_emissions(&pool), 3 * SCALAR_7);
        assert_eq!(
            fixture.client().pool_emission_reservation(&pool),
            PoolEmissionReservation {
                available: 3 * SCALAR_7,
                last_gulp: Some(1_010),
            }
        );
        assert!(fixture.client().try_gulp_pool_emissions(&pool).is_err());

        fixture
            .client()
            .claim_pool_emissions(&pool, &recipient, &SCALAR_7);
        assert_eq!(
            fixture.client().pool_emission_reservation(&pool).available,
            2 * SCALAR_7
        );
        assert_eq!(
            TokenClient::new(&fixture.e, &fixture.blnd).balance(&recipient),
            SCALAR_7
        );
        assert_eq!(
            fixture.client().ongoing_emission_state().total_claimed,
            SCALAR_7
        );
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
        assert!(!fixture.client().blnd_binding_verified());
        assert_eq!(
            fixture.client().ongoing_emission_state(),
            OngoingEmissionState {
                backstop_allocated: 0,
                backstop_carry: 0,
                backstop_claimed: 0,
                last_distribution: Some(1_000),
                pool_allocated: 0,
                pool_carry: 0,
                split_carry: 0,
                total_claimed: 0,
                total_received: 0,
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

        let distribution = fixture.client().distribute();
        assert_eq!(distribution.received, 5 * SCALAR_7);
        assert_eq!(distribution.eligible_blnd, 300 * SCALAR_7);
        for pool in pools.iter() {
            let emissions = fixture.client().pool_ongoing_emissions(&pool);
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
            fixture.client().distribute(),
            OngoingDistribution {
                backstop_allocated: 0,
                backstop_carry: 35_000_000,
                checkpoint: 1_005,
                eligible_blnd: 0,
                pool_allocated: 0,
                pool_carry: 15_000_000,
                received: 5 * SCALAR_7,
                split_carry: 0,
            }
        );

        fixture.client().deposit(
            &crate::BackstopTier::BlndUsdc,
            &fixture.admin,
            &pool,
            &SCALAR_7,
        );
        assert_eq!(
            fixture
                .client()
                .pool_ongoing_emissions(&pool)
                .active_blnd_usdc,
            SCALAR_7
        );

        fixture.e.ledger().set_timestamp(1_010);
        assert_eq!(
            fixture.client().distribute(),
            OngoingDistribution {
                backstop_allocated: 7 * SCALAR_7,
                backstop_carry: 0,
                checkpoint: 1_010,
                eligible_blnd: 10 * SCALAR_7,
                pool_allocated: 3 * SCALAR_7,
                pool_carry: 0,
                received: 5 * SCALAR_7,
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
        assert_eq!(
            fixture
                .client()
                .pool_ongoing_emissions(&pool)
                .active_blnd_usdc,
            9 * SCALAR_7
        );
    }
}
