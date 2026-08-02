use crate::{
    backstop::{
        self, build_pool_valuation, load_pool_backstop_data, load_pool_tier_state, preview_deposit,
        preview_withdrawal, quote_activation, quote_status_set, quote_status_update, tier_token,
        user_queued_shares, user_total_shares, validate_backstop_assets, ActivationQuote,
        ActivationValues, BackstopTier, BadDebtLotQuote, BlndEmissionValues, InterestLotQuote,
        PoolBackstopData, PoolStatusQuote, PoolTierState, PoolValuation, TakeRateQuote,
        TakeRateValues, TierTotals, Q4W,
    },
    constants::{ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC},
    dependencies::PoolFactoryClient,
    emissions::{
        self, BlndEmissionQuote, OngoingBlndSplit, OngoingDistribution, OngoingEmissionState,
        PoolEmissionReservation, PoolOngoingEmissions, RewardZoneCheckpoint, UserOngoingEmissions,
    },
    errors::BackstopError,
    events::BackstopEvents,
    migration::{self, MigrationStatus},
    storage,
};
use soroban_sdk::{
    contract, contractclient, contractimpl, panic_with_error, Address, BytesN, Env, Map, Vec,
};

/// ### Backstop
///
/// A backstop module for the Blend protocol's Isolated Lending Pools
#[contract]
pub struct BackstopContract;

#[contractclient(name = "BackstopClient")]
pub trait Backstop {
    /********** Core **********/

    /// Deposit the token bound to `tier`.
    fn deposit(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> i128;

    /// Queue shares from `tier` for withdrawal.
    fn queue_withdrawal(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> Q4W;

    /// Restore queued shares from `tier`.
    fn dequeue_withdrawal(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    );

    /// Withdraw expired shares from `tier`.
    fn withdraw(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128;

    /// Fetch the backstop data for the pool
    ///
    /// Return a summary of the pool's backstop data
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool
    fn pool_data(e: Env, pool: Address) -> PoolBackstopData;

    /// Fetch the token contract bound to one fixed backstop tier.
    fn tier_token(e: Env, tier: BackstopTier) -> Address;

    /// Fetch one user's total active and queued shares in a tier.
    fn tier_shares(e: Env, tier: BackstopTier, user: Address, pool: Address) -> i128;

    /// Fetch one user's queued shares in a tier.
    fn tier_queued_shares(e: Env, tier: BackstopTier, user: Address, pool: Address) -> i128;

    /// Fetch one user's currently active shares in a tier.
    fn tier_active_shares(e: Env, tier: BackstopTier, user: Address, pool: Address) -> i128;

    /// Fetch one user's bounded withdrawal queue in a tier.
    fn tier_withdrawal_queue(e: Env, tier: BackstopTier, user: Address, pool: Address) -> Vec<Q4W>;

    /// Fetch a pool's complete state for one tier.
    fn pool_tier_state(e: Env, tier: BackstopTier, pool: Address) -> PoolTierState;

    /// Fetch aggregate accounting totals for one tier.
    fn tier_totals(e: Env, tier: BackstopTier) -> TierTotals;

    /// Preview shares minted by a tier deposit.
    fn preview_tier_deposit(e: Env, tier: BackstopTier, pool: Address, amount: i128) -> i128;

    /// Preview tokens returned by a tier withdrawal.
    fn preview_tier_withdrawal(e: Env, tier: BackstopTier, pool: Address, shares: i128) -> i128;

    /// Return the BLND:USDC token through the v2-compatible getter.
    fn backstop_token(e: Env) -> Address;

    /// Return the candidate's legacy-emitter migration state.
    fn migration_status(e: Env) -> MigrationStatus;

    fn prefunding_start(e: Env) -> u64;

    fn absolute_migration_deadline(e: Env) -> u64;

    fn migration_epoch_start(e: Env) -> Option<u64>;

    fn original_unlock(e: Env) -> Option<u64>;

    fn verified_queue_unlock(e: Env) -> Option<u64>;

    fn retry_count(e: Env) -> u32;

    fn activated_at(e: Env) -> Option<u64>;

    fn backfill_cap(e: Env) -> Option<u64>;

    fn backfill_end(e: Env) -> Option<u64>;

    fn sync_deadline(e: Env) -> Option<u64>;

    fn scheduled_backfill(e: Env) -> i128;

    fn funded_backfill(e: Env) -> Option<i128>;

    /// Atomically queue the candidate in the legacy emitter and open its epoch.
    fn begin_migration(e: Env) -> u64;

    /// Record a correct queue created directly through the legacy emitter.
    fn open_migration_epoch(e: Env) -> u64;

    /// Verify the current queue during its final seven-day preparation window.
    fn prepare_migration(e: Env) -> u64;

    /// Atomically execute the legacy-emitter swap and activate v3 accounting.
    fn finalize_migration(e: Env) -> u64;

    /// Synchronize after another caller directly executes the prepared swap.
    fn sync_migration(e: Env) -> u64;

    /// Fund exactly the scheduled migration backfill through the emitter drop.
    fn fund_backfill(e: Env) -> i128;

    /// Fetch the reward zone for the backstop
    fn reward_zone(e: Env) -> Vec<Address>;

    /// Return the most recent completed distribution checkpoint, if any.
    fn reward_zone_checkpoint(e: Env) -> Option<RewardZoneCheckpoint>;

    /// Return the verified USDC entry threshold for inactive pools.
    fn activation_entry_threshold(e: Env) -> i128;

    /// Return the verified USDC maintenance threshold for active pools.
    fn activation_maintenance_threshold(e: Env) -> i128;

    /// Quote activation policy arithmetic for verified tier values.
    fn quote_activation(
        e: Env,
        values: ActivationValues,
        currently_active: bool,
    ) -> ActivationQuote;

    /// Return one registered pool's canonical active and queued valuation.
    fn pool_valuation(e: Env, pool: Address) -> PoolValuation;

    /// Quote activation from canonical pool accounting and valuation.
    fn quote_pool_activation(e: Env, pool: Address, currently_active: bool) -> ActivationQuote;

    /// Quote a permissionless pool-status refresh.
    fn quote_pool_status_update(e: Env, pool: Address, current_status: u32) -> PoolStatusQuote;

    /// Quote a pool-admin status request.
    fn quote_pool_status_set(
        e: Env,
        pool: Address,
        current_status: u32,
        requested_status: u32,
    ) -> PoolStatusQuote;

    /// Quote the first qualifying single-tier bad-debt lot.
    fn quote_bad_debt_lot(e: Env, pool: Address, debt_value: i128) -> Option<BadDebtLotQuote>;

    /// Reserve one pool-authorized single-tier bad-debt lot.
    fn commit_bad_debt_lot(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
        debt_value: i128,
    ) -> BadDebtLotQuote;

    /// Release a pool-authorized bad-debt commitment.
    fn release_bad_debt_lot(e: Env, pool: Address, auction_id: BytesN<32>);

    /// Settle one partial or complete fill of a committed bad-debt lot.
    fn settle_bad_debt_lot(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
        base_lot_amount: i128,
        lot_amount: i128,
        to: Address,
    ) -> Option<BadDebtLotQuote>;

    /// Return token units reserved by a pool in one tier.
    fn pool_tier_committed_assets(e: Env, tier: BackstopTier, pool: Address) -> i128;

    /// Return the bounded number of active commitments for a pool.
    fn pool_bad_debt_commitment_count(e: Env, pool: Address) -> u32;

    /// Return one matching commitment quote.
    fn bad_debt_commitment(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
    ) -> Option<BadDebtLotQuote>;

    /// Quote one reserve-credit allocation from verified tier values.
    fn quote_take_rate(e: Env, distribution: i128, values: TakeRateValues) -> TakeRateQuote;

    /// Allocate a bounded reserve-credit batch from canonical pool-tier value.
    fn quote_pool_take_rate_batch(
        e: Env,
        pool: Address,
        distributions: Map<Address, i128>,
    ) -> Map<Address, TakeRateQuote>;

    /// Reserve the selected tier-token bid for one pool interest auction.
    fn commit_interest_lot(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
        tier: BackstopTier,
        lot_value: i128,
    ) -> InterestLotQuote;

    /// Release a pool-authorized interest commitment for one tier.
    fn release_interest_lot(e: Env, pool: Address, tier: BackstopTier, auction_id: BytesN<32>);

    /// Donate one time-scaled bid and resize or complete its commitment.
    fn settle_interest_lot(
        e: Env,
        pool: Address,
        tier: BackstopTier,
        auction_id: BytesN<32>,
        base_bid_amount: i128,
        bid_amount: i128,
        from: Address,
    ) -> Option<InterestLotQuote>;

    /// Return one matching interest commitment quote.
    fn interest_commitment(
        e: Env,
        pool: Address,
        tier: BackstopTier,
        auction_id: BytesN<32>,
    ) -> Option<InterestLotQuote>;

    /********** Emissions **********/

    /// Quote a reward-zone pool's share of the backstop-depositor BLND tranche.
    ///
    /// Production accrual derives the values from canonical active tier shares
    /// and current Comet composition.
    fn quote_pool_blnd_emissions(
        e: Env,
        distribution: i128,
        values: BlndEmissionValues,
        total_reward_zone_blnd: i128,
        reward_zone_member: bool,
    ) -> BlndEmissionQuote;

    /// Quote one user's share of a reward-zone pool's BLND allocation.
    ///
    /// Production accrual derives the values from canonical active tier shares
    /// and current Comet composition.
    fn quote_user_blnd_emissions(
        e: Env,
        pool_distribution: i128,
        values: BlndEmissionValues,
        pool_eligible_blnd: i128,
    ) -> BlndEmissionQuote;

    /// Convert LP amounts to underlying BLND using current Comet composition.
    fn spot_blnd_emission_values(
        e: Env,
        blnd_usdc_lp: i128,
        blnd_xlm_lp: i128,
    ) -> BlndEmissionValues;

    /// Return one registered pool's current active BLND-emission weight.
    fn pool_spot_blnd_emission_values(e: Env, pool: Address) -> BlndEmissionValues;

    /// Quote the immutable 70% backstop / 30% pool split with carry.
    fn quote_ongoing_blnd_split(e: Env, distribution: i128, prior_carry: i128) -> OngoingBlndSplit;

    /// Allocate the next migration-backfill or ongoing BLND checkpoint.
    fn distribute(e: Env) -> OngoingDistribution;

    /// Return aggregate BLND obligations, allocations, claims, and carries.
    fn ongoing_emission_state(e: Env) -> OngoingEmissionState;

    /// Return one pool's ongoing BLND allocation and active tier amounts.
    fn pool_ongoing_emissions(e: Env, pool: Address) -> PoolOngoingEmissions;

    /// Return whether emitter output has bound the configured BLND token.
    fn blnd_binding_verified(e: Env) -> bool;

    /// Return one user's pending BLND, including migration backfill.
    fn user_ongoing_emissions(
        e: Env,
        user: Address,
        pool: Address,
        tier: BackstopTier,
    ) -> UserOngoingEmissions;

    /// Compound one eligible tier's accrued BLND into that tier's Comet LP.
    fn claim_ongoing_blnd(
        e: Env,
        tier: BackstopTier,
        user: Address,
        pool: Address,
        min_lp_tokens_out: i128,
    ) -> i128;

    /// Return one pool's reserved, unclaimed 30% tranche.
    fn pool_emission_reservation(e: Env, pool: Address) -> PoolEmissionReservation;

    /// Move one pool's accrued 30% tranche into its claim reservation.
    fn gulp_pool_emissions(e: Env, pool: Address) -> i128;

    /// Pay an authorized reserve-token claim from one pool's reservation.
    fn claim_pool_emissions(e: Env, pool: Address, recipient: Address, amount: i128);

    /// Add a threshold-qualified pool with positive active BLND to the reward zone.
    ///
    /// If all 30 slots are occupied, the replacement must have strictly more
    /// active underlying BLND than the named member.
    ///
    /// ### Arguments
    /// * `to_add` - The address of the pool to add
    /// * `to_remove` - The address of the pool to remove (Optional - Used if the reward zone is full)
    ///
    /// ### Errors
    /// If the pool is ineligible or the required distribution checkpoint is stale
    fn add_reward(e: Env, to_add: Address, to_remove: Option<Address>);

    /// Remove a pool below the maintenance threshold from the reward zone.
    ///
    /// A member with zero active underlying BLND can be removed regardless of
    /// activation value and without a recent distribution checkpoint.
    ///
    /// ### Arguments
    /// * `to_remove` - The address of the pool to remove
    ///
    /// ### Errors
    /// If the pool is not below the threshold or if the pool is not in the reward zone
    fn remove_reward(e: Env, to_remove: Address);

    /********** Fund Management *********/

    /// (Only Pool) Take backstop token from a pools backstop
    ///
    /// ### Arguments
    /// * `from` - The address of the pool drawing tokens from the backstop
    /// * `pool_address` - The address of the pool
    /// * `amount` - The amount of backstop tokens to draw
    /// * `to` - The address to send the backstop tokens to
    ///
    /// ### Errors
    /// If the pool does not have enough backstop tokens, or if the pool does
    /// not authorize the call
    fn draw(e: Env, pool_address: Address, amount: i128, to: Address);

    /// (Only Pool) Sends backstop tokens from `from` to a pools backstop
    ///
    /// NOTE: This is not a deposit, and `from` will permanently lose access to the funds
    ///
    /// ### Arguments
    /// * `from` - The address of the pool donating tokens to the backstop
    /// * `pool_address` - The address of the pool
    /// * `amount` - The amount of BLND to add
    ///
    /// ### Errors
    /// If the `pool_address` is not valid, backstop does not have sufficient allowance from `from`, or if the pool does not
    /// authorize the call
    fn donate(e: Env, from: Address, pool_address: Address, amount: i128);
}

#[contractimpl]
impl BackstopContract {
    /// Construct the backstop contract
    ///
    /// ### Arguments
    /// * `blnd_usdc_token` - The second-loss LP token with the pair BLND:USDC
    /// * `blnd_xlm_token` - The first-loss LP token with the pair BLND:XLM
    /// * `emitter` - The Emitter contract ID
    /// * `blnd_token` - The BLND token ID
    /// * `usdc_token` - The USDC token ID
    /// * `xlm_token` - The XLM token ID
    /// * `pool_factory` - The pool factory ID
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        e: Env,
        blnd_usdc_token: Address,
        blnd_xlm_token: Address,
        emitter: Address,
        blnd_token: Address,
        usdc_token: Address,
        xlm_token: Address,
        pool_factory: Address,
    ) {
        validate_backstop_assets(
            &e,
            &blnd_token,
            &usdc_token,
            &xlm_token,
            &blnd_usdc_token,
            &blnd_xlm_token,
        );
        let factory = PoolFactoryClient::new(&e, &pool_factory);
        if factory.backstop() != e.current_contract_address() {
            panic_with_error!(&e, BackstopError::InvalidPoolFactoryBinding);
        }
        let _ = factory.is_pool(&e.current_contract_address());
        storage::set_blnd_usdc_token(&e, &blnd_usdc_token);
        storage::set_blnd_xlm_token(&e, &blnd_xlm_token);
        storage::set_blnd_token(&e, &blnd_token);
        storage::set_usdc_token(&e, &usdc_token);
        storage::set_xlm_token(&e, &xlm_token);
        storage::set_pool_factory(&e, &pool_factory);
        storage::set_emitter(&e, &emitter);
        migration::initialize(&e);
        storage::extend_instance(&e);
    }
}

/// @dev
/// The contract implementation only manages the authorization / authentication required from the caller(s), and
/// utilizes other modules to carry out contract functionality.
#[contractimpl]
impl Backstop for BackstopContract {
    /********** Core **********/

    fn deposit(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> i128 {
        deposit_tier(e, tier, from, pool_address, amount)
    }

    fn queue_withdrawal(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> Q4W {
        queue_tier_withdrawal(e, tier, from, pool_address, amount)
    }

    fn dequeue_withdrawal(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) {
        dequeue_tier_withdrawal(e, tier, from, pool_address, amount)
    }

    fn withdraw(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128 {
        withdraw_tier(e, tier, from, pool_address, amount, to)
    }

    fn pool_data(e: Env, pool: Address) -> PoolBackstopData {
        load_pool_backstop_data(&e, &pool)
    }

    fn tier_token(e: Env, tier: BackstopTier) -> Address {
        tier_token(&e, tier)
    }

    fn tier_shares(e: Env, tier: BackstopTier, user: Address, pool: Address) -> i128 {
        let balance = storage::get_user_balance_for_tier(&e, tier, &pool, &user);
        user_total_shares(&balance)
    }

    fn tier_queued_shares(e: Env, tier: BackstopTier, user: Address, pool: Address) -> i128 {
        let balance = storage::get_user_balance_for_tier(&e, tier, &pool, &user);
        user_queued_shares(&balance)
    }

    fn tier_active_shares(e: Env, tier: BackstopTier, user: Address, pool: Address) -> i128 {
        storage::get_user_balance_for_tier(&e, tier, &pool, &user).shares
    }

    fn tier_withdrawal_queue(e: Env, tier: BackstopTier, user: Address, pool: Address) -> Vec<Q4W> {
        storage::get_user_balance_for_tier(&e, tier, &pool, &user).q4w
    }

    fn pool_tier_state(e: Env, tier: BackstopTier, pool: Address) -> PoolTierState {
        load_pool_tier_state(&e, tier, &pool)
    }

    fn tier_totals(e: Env, tier: BackstopTier) -> TierTotals {
        storage::get_tier_totals(&e, tier)
    }

    fn preview_tier_deposit(e: Env, tier: BackstopTier, pool: Address, amount: i128) -> i128 {
        require_nonnegative(&e, amount);
        preview_deposit(&storage::get_pool_balance_for_tier(&e, tier, &pool), amount)
    }

    fn preview_tier_withdrawal(e: Env, tier: BackstopTier, pool: Address, shares: i128) -> i128 {
        require_nonnegative(&e, shares);
        preview_withdrawal(&storage::get_pool_balance_for_tier(&e, tier, &pool), shares)
    }

    fn backstop_token(e: Env) -> Address {
        storage::get_blnd_usdc_token(&e)
    }

    fn migration_status(e: Env) -> MigrationStatus {
        storage::extend_instance(&e);
        migration::status(&e)
    }

    fn prefunding_start(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::prefunding_start(&e)
    }

    fn absolute_migration_deadline(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::absolute_migration_deadline(&e)
    }

    fn migration_epoch_start(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::migration_epoch_start(&e)
    }

    fn original_unlock(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::original_unlock(&e)
    }

    fn verified_queue_unlock(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::verified_queue_unlock(&e)
    }

    fn retry_count(e: Env) -> u32 {
        storage::extend_instance(&e);
        migration::retry_count(&e)
    }

    fn activated_at(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::activated_at(&e)
    }

    fn backfill_cap(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::backfill_cap(&e)
    }

    fn backfill_end(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::backfill_end(&e)
    }

    fn sync_deadline(e: Env) -> Option<u64> {
        storage::extend_instance(&e);
        migration::sync_deadline(&e)
    }

    fn scheduled_backfill(e: Env) -> i128 {
        storage::extend_instance(&e);
        migration::scheduled_backfill(&e)
    }

    fn funded_backfill(e: Env) -> Option<i128> {
        storage::extend_instance(&e);
        migration::funded_backfill(&e)
    }

    fn begin_migration(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::begin_migration(&e)
    }

    fn open_migration_epoch(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::open_migration_epoch(&e)
    }

    fn prepare_migration(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::prepare_migration(&e)
    }

    fn finalize_migration(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::finalize_migration(&e)
    }

    fn sync_migration(e: Env) -> u64 {
        storage::extend_instance(&e);
        migration::sync_migration(&e)
    }

    fn fund_backfill(e: Env) -> i128 {
        storage::extend_instance(&e);
        migration::fund_backfill(&e)
    }

    fn reward_zone(e: Env) -> Vec<Address> {
        storage::extend_instance(&e);
        emissions::get_reward_zone(&e)
    }

    fn reward_zone_checkpoint(e: Env) -> Option<RewardZoneCheckpoint> {
        storage::extend_instance(&e);
        emissions::get_reward_zone_checkpoint(&e)
    }

    fn activation_entry_threshold(_e: Env) -> i128 {
        ACTIVATION_ENTRY_THRESHOLD_USDC
    }

    fn activation_maintenance_threshold(_e: Env) -> i128 {
        ACTIVATION_MAINTENANCE_THRESHOLD_USDC
    }

    fn quote_activation(
        e: Env,
        values: ActivationValues,
        currently_active: bool,
    ) -> ActivationQuote {
        quote_activation(&e, &values, currently_active)
    }

    fn pool_valuation(e: Env, pool: Address) -> PoolValuation {
        build_pool_valuation(&e, &pool)
    }

    fn quote_pool_activation(e: Env, pool: Address, currently_active: bool) -> ActivationQuote {
        let valuation = build_pool_valuation(&e, &pool);
        quote_activation(&e, &valuation.active_values, currently_active)
    }

    fn quote_pool_status_update(e: Env, pool: Address, current_status: u32) -> PoolStatusQuote {
        let valuation = build_pool_valuation(&e, &pool);
        quote_status_update(
            &e,
            current_status,
            &valuation.active_values,
            &valuation.queued_values,
        )
    }

    fn quote_pool_status_set(
        e: Env,
        pool: Address,
        current_status: u32,
        requested_status: u32,
    ) -> PoolStatusQuote {
        let valuation = build_pool_valuation(&e, &pool);
        quote_status_set(
            &e,
            current_status,
            requested_status,
            &valuation.active_values,
            &valuation.queued_values,
        )
    }

    fn quote_bad_debt_lot(e: Env, pool: Address, debt_value: i128) -> Option<BadDebtLotQuote> {
        storage::extend_instance(&e);
        backstop::quote_bad_debt_lot(&e, &pool, debt_value)
    }

    fn commit_bad_debt_lot(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
        debt_value: i128,
    ) -> BadDebtLotQuote {
        storage::extend_instance(&e);
        pool.require_auth();
        let quote = backstop::commit_bad_debt_lot(&e, &pool, &auction_id, debt_value);
        BackstopEvents::bad_debt_lot_committed(&e, pool, auction_id, quote.clone());
        quote
    }

    fn release_bad_debt_lot(e: Env, pool: Address, auction_id: BytesN<32>) {
        storage::extend_instance(&e);
        pool.require_auth();
        backstop::release_bad_debt_lot(&e, &pool, &auction_id);
        BackstopEvents::bad_debt_lot_released(&e, pool, auction_id);
    }

    fn settle_bad_debt_lot(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
        base_lot_amount: i128,
        lot_amount: i128,
        to: Address,
    ) -> Option<BadDebtLotQuote> {
        storage::extend_instance(&e);
        pool.require_auth();
        let tier = backstop::bad_debt_commitment(&e, &pool, &auction_id)
            .unwrap_or_else(|| panic_with_error!(&e, BackstopError::BadDebtCommitmentNotFound))
            .tier;
        let remaining =
            backstop::settle_bad_debt_lot(&e, &pool, &auction_id, base_lot_amount, lot_amount, &to);
        BackstopEvents::bad_debt_lot_settled(
            &e,
            pool,
            auction_id,
            base_lot_amount,
            lot_amount,
            to,
            tier,
            remaining.is_none(),
        );
        remaining
    }

    fn pool_tier_committed_assets(e: Env, tier: BackstopTier, pool: Address) -> i128 {
        storage::extend_instance(&e);
        backstop::pool_tier_committed_assets(&e, tier, &pool)
    }

    fn pool_bad_debt_commitment_count(e: Env, pool: Address) -> u32 {
        storage::extend_instance(&e);
        backstop::pool_bad_debt_commitment_count(&e, &pool)
    }

    fn bad_debt_commitment(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
    ) -> Option<BadDebtLotQuote> {
        storage::extend_instance(&e);
        backstop::bad_debt_commitment(&e, &pool, &auction_id)
    }

    fn quote_take_rate(e: Env, distribution: i128, values: TakeRateValues) -> TakeRateQuote {
        storage::extend_instance(&e);
        backstop::quote_take_rate(&e, distribution, &values)
    }

    fn quote_pool_take_rate_batch(
        e: Env,
        pool: Address,
        distributions: Map<Address, i128>,
    ) -> Map<Address, TakeRateQuote> {
        storage::extend_instance(&e);
        backstop::quote_pool_take_rate_batch(&e, &pool, &distributions)
    }

    fn commit_interest_lot(
        e: Env,
        pool: Address,
        auction_id: BytesN<32>,
        tier: BackstopTier,
        lot_value: i128,
    ) -> InterestLotQuote {
        storage::extend_instance(&e);
        pool.require_auth();
        let quote = backstop::commit_interest_lot(&e, &pool, &auction_id, tier, lot_value);
        BackstopEvents::interest_lot_committed(&e, pool, auction_id, quote.clone());
        quote
    }

    fn release_interest_lot(e: Env, pool: Address, tier: BackstopTier, auction_id: BytesN<32>) {
        storage::extend_instance(&e);
        pool.require_auth();
        backstop::release_interest_lot(&e, &pool, tier, &auction_id);
        BackstopEvents::interest_lot_released(&e, pool, auction_id);
    }

    fn settle_interest_lot(
        e: Env,
        pool: Address,
        tier: BackstopTier,
        auction_id: BytesN<32>,
        base_bid_amount: i128,
        bid_amount: i128,
        from: Address,
    ) -> Option<InterestLotQuote> {
        storage::extend_instance(&e);
        pool.require_auth();
        let remaining = backstop::settle_interest_lot(
            &e,
            &pool,
            tier,
            &auction_id,
            base_bid_amount,
            bid_amount,
            &from,
        );
        BackstopEvents::interest_lot_settled(
            &e,
            pool,
            auction_id,
            base_bid_amount,
            bid_amount,
            from,
            tier,
            remaining.is_none(),
        );
        remaining
    }

    fn interest_commitment(
        e: Env,
        pool: Address,
        tier: BackstopTier,
        auction_id: BytesN<32>,
    ) -> Option<InterestLotQuote> {
        storage::extend_instance(&e);
        backstop::interest_commitment(&e, &pool, tier, &auction_id)
    }

    /********** Emissions **********/

    fn quote_pool_blnd_emissions(
        e: Env,
        distribution: i128,
        values: BlndEmissionValues,
        total_reward_zone_blnd: i128,
        reward_zone_member: bool,
    ) -> BlndEmissionQuote {
        storage::extend_instance(&e);
        emissions::quote_pool_blnd_emissions(
            &e,
            distribution,
            &values,
            total_reward_zone_blnd,
            reward_zone_member,
        )
    }

    fn quote_user_blnd_emissions(
        e: Env,
        pool_distribution: i128,
        values: BlndEmissionValues,
        pool_eligible_blnd: i128,
    ) -> BlndEmissionQuote {
        storage::extend_instance(&e);
        emissions::quote_user_blnd_emissions(&e, pool_distribution, &values, pool_eligible_blnd)
    }

    fn spot_blnd_emission_values(
        e: Env,
        blnd_usdc_lp: i128,
        blnd_xlm_lp: i128,
    ) -> BlndEmissionValues {
        storage::extend_instance(&e);
        emissions::spot_blnd_emission_values(&e, blnd_usdc_lp, blnd_xlm_lp)
    }

    fn pool_spot_blnd_emission_values(e: Env, pool: Address) -> BlndEmissionValues {
        storage::extend_instance(&e);
        backstop::require_registered_pool(&e, &pool);
        emissions::pool_spot_blnd_emission_values(&e, &pool)
    }

    fn quote_ongoing_blnd_split(e: Env, distribution: i128, prior_carry: i128) -> OngoingBlndSplit {
        storage::extend_instance(&e);
        emissions::quote_ongoing_blnd_split(&e, distribution, prior_carry)
    }

    fn distribute(e: Env) -> OngoingDistribution {
        storage::extend_instance(&e);
        let distribution = emissions::distribute(&e);

        BackstopEvents::distribute(&e, distribution.distributed);
        distribution
    }

    fn ongoing_emission_state(e: Env) -> OngoingEmissionState {
        storage::extend_instance(&e);
        emissions::get_ongoing_emission_state(&e)
    }

    fn pool_ongoing_emissions(e: Env, pool: Address) -> PoolOngoingEmissions {
        storage::extend_instance(&e);
        backstop::require_registered_pool(&e, &pool);
        emissions::get_pool_ongoing_emissions(&e, &pool)
    }

    fn blnd_binding_verified(e: Env) -> bool {
        storage::extend_instance(&e);
        storage::get_blnd_binding_verified(&e)
    }

    fn user_ongoing_emissions(
        e: Env,
        user: Address,
        pool: Address,
        tier: BackstopTier,
    ) -> UserOngoingEmissions {
        storage::extend_instance(&e);
        backstop::require_registered_pool(&e, &pool);
        emissions::preview_user_ongoing_emissions(&e, tier, &user, &pool)
    }

    fn claim_ongoing_blnd(
        e: Env,
        tier: BackstopTier,
        user: Address,
        pool: Address,
        min_lp_tokens_out: i128,
    ) -> i128 {
        storage::extend_instance(&e);
        let claim = emissions::claim_user_ongoing_blnd(&e, tier, &user, &pool, min_lp_tokens_out);
        BackstopEvents::claim_ongoing_blnd(
            &e,
            tier,
            user,
            pool,
            claim.blnd_amount,
            claim.lp_amount,
            claim.shares,
        );
        claim.lp_amount
    }

    fn pool_emission_reservation(e: Env, pool: Address) -> PoolEmissionReservation {
        storage::extend_instance(&e);
        backstop::require_registered_pool(&e, &pool);
        emissions::get_pool_emission_reservation(&e, &pool)
    }

    fn gulp_pool_emissions(e: Env, pool: Address) -> i128 {
        storage::extend_instance(&e);
        let amount = emissions::gulp_pool_ongoing_emissions(&e, &pool);
        BackstopEvents::gulp_emissions(&e, pool, 0, amount);
        amount
    }

    fn claim_pool_emissions(e: Env, pool: Address, recipient: Address, amount: i128) {
        storage::extend_instance(&e);
        emissions::claim_reserved_pool_emissions(&e, &pool, &recipient, amount);
        BackstopEvents::claim_pool_emissions(&e, pool, recipient, amount);
    }

    fn add_reward(e: Env, to_add: Address, to_remove: Option<Address>) {
        storage::extend_instance(&e);
        let removed = emissions::add_to_reward_zone(&e, &to_add, to_remove.as_ref());

        BackstopEvents::rw_zone_add(&e, to_add, removed);
    }

    fn remove_reward(e: Env, to_remove: Address) {
        storage::extend_instance(&e);
        emissions::remove_from_reward_zone(&e, &to_remove);

        BackstopEvents::rw_zone_remove(&e, to_remove);
    }

    /********** Fund Management *********/

    fn draw(e: Env, pool_address: Address, amount: i128, to: Address) {
        storage::extend_instance(&e);
        pool_address.require_auth();

        backstop::execute_draw(&e, &pool_address, amount, &to);

        BackstopEvents::draw(&e, pool_address, to, amount);
    }

    fn donate(e: Env, from: Address, pool_address: Address, amount: i128) {
        storage::extend_instance(&e);
        from.require_auth();
        pool_address.require_auth();

        backstop::execute_donate(&e, &from, &pool_address, amount);

        BackstopEvents::donate(&e, pool_address, from, amount);
    }
}

fn deposit_tier(
    e: Env,
    tier: BackstopTier,
    from: Address,
    pool_address: Address,
    amount: i128,
) -> i128 {
    storage::extend_instance(&e);
    from.require_auth();
    let shares = backstop::execute_deposit_for_tier(&e, tier, &from, &pool_address, amount);
    BackstopEvents::tier_deposit(&e, tier, pool_address, from, amount, shares);
    shares
}

fn queue_tier_withdrawal(
    e: Env,
    tier: BackstopTier,
    from: Address,
    pool_address: Address,
    amount: i128,
) -> Q4W {
    storage::extend_instance(&e);
    from.require_auth();
    let entry = backstop::execute_queue_withdrawal_for_tier(&e, tier, &from, &pool_address, amount);
    BackstopEvents::tier_queue_withdrawal(&e, tier, pool_address, from, amount, entry.exp);
    entry
}

fn dequeue_tier_withdrawal(
    e: Env,
    tier: BackstopTier,
    from: Address,
    pool_address: Address,
    amount: i128,
) {
    storage::extend_instance(&e);
    from.require_auth();
    backstop::execute_dequeue_withdrawal_for_tier(&e, tier, &from, &pool_address, amount);
    BackstopEvents::tier_dequeue_withdrawal(&e, tier, pool_address, from, amount);
}

fn withdraw_tier(
    e: Env,
    tier: BackstopTier,
    from: Address,
    pool_address: Address,
    amount: i128,
    to: Address,
) -> i128 {
    storage::extend_instance(&e);
    from.require_auth();
    let tokens = backstop::execute_withdraw_for_tier(&e, tier, &from, &pool_address, amount, &to);
    BackstopEvents::tier_withdraw(&e, tier, pool_address, from, amount, tokens);
    tokens
}

/// Require that an incoming amount is not negative
///
/// ### Arguments
/// * `amount` - The amount
///
/// ### Errors
/// If the number is negative
pub fn require_nonnegative(e: &Env, amount: i128) {
    if amount.is_negative() {
        panic_with_error!(e, BackstopError::NegativeAmountError);
    }
}
