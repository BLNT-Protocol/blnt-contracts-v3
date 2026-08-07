use crate::{
    backstop::{
        self, build_pool_data, preview_deposit, preview_withdrawal, tier_token,
        validate_backstop_assets, BackstopTier, BadDebtLotQuote, InterestLotQuote, PoolData,
        TakeRateQuote, UserBalance, Q4W,
    },
    constants::{ACTIVATION_ENTRY_THRESHOLD_USDC, ACTIVATION_MAINTENANCE_THRESHOLD_USDC},
    dependencies::PoolFactoryClient,
    emissions::{
        self, OngoingEmissionState, PoolOngoingEmissions, RewardZoneCheckpoint,
        UserOngoingEmissions,
    },
    errors::BackstopError,
    events::BackstopEvents,
    migration::{self, MigrationState},
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
    /// Return the pool's complete three-tier accounting and valuation.
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool
    fn pool_data(e: Env, pool: Address) -> PoolData;

    /// Fetch the token contract bound to one fixed backstop tier.
    fn backstop_token(e: Env, tier: BackstopTier) -> Address;

    /// Fetch one user's active shares and bounded withdrawal queue in a tier.
    fn user_balance(e: Env, tier: BackstopTier, pool: Address, user: Address) -> UserBalance;

    /// Preview shares minted by a tier deposit.
    fn preview_tier_deposit(e: Env, tier: BackstopTier, pool: Address, amount: i128) -> i128;

    /// Preview tokens returned by a tier withdrawal.
    fn preview_tier_withdrawal(e: Env, tier: BackstopTier, pool: Address, shares: i128) -> i128;

    /// Return the complete incumbent-emitter migration state.
    fn migration_state(e: Env) -> MigrationState;

    /// Fund the scheduled migration backfill through the emitter's v2 drop.
    fn drop(e: Env);

    /// Fetch the reward zone for the backstop
    fn reward_zone(e: Env) -> Vec<Address>;

    /// Return the most recent completed distribution checkpoint, if any.
    fn reward_zone_checkpoint(e: Env) -> Option<RewardZoneCheckpoint>;

    /// Return the verified USDC entry threshold for inactive pools.
    fn activation_entry_threshold(e: Env) -> i128;

    /// Return the verified USDC maintenance threshold for active pools.
    fn activation_maintenance_threshold(e: Env) -> i128;

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

    /********** Emissions **********/

    /// Allocate the next migration-backfill or ongoing BLND checkpoint.
    fn distribute(e: Env) -> i128;

    /// Return aggregate BLND allocations, backstop claims, and carries.
    fn ongoing_emission_state(e: Env) -> OngoingEmissionState;

    /// Return one pool's ongoing BLND allocation and active tier amounts.
    fn pool_ongoing_emissions(e: Env, pool: Address) -> PoolOngoingEmissions;

    /// Return one user's pending BLND, including migration backfill.
    fn user_ongoing_emissions(
        e: Env,
        user: Address,
        pool: Address,
        tier: BackstopTier,
    ) -> UserOngoingEmissions;

    /// Compound one eligible tier's accrued BLND into that tier's Comet LP.
    fn claim(
        e: Env,
        tier: BackstopTier,
        user: Address,
        pool: Address,
        min_lp_tokens_out: i128,
    ) -> i128;

    /// Grant one pool an allowance for its accrued 30% tranche.
    fn gulp_emissions(e: Env, pool: Address) -> i128;

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

    fn pool_data(e: Env, pool: Address) -> PoolData {
        build_pool_data(&e, &pool)
    }

    fn backstop_token(e: Env, tier: BackstopTier) -> Address {
        tier_token(&e, tier)
    }

    fn user_balance(e: Env, tier: BackstopTier, pool: Address, user: Address) -> UserBalance {
        storage::get_user_balance_for_tier(&e, tier, &pool, &user)
    }

    fn preview_tier_deposit(e: Env, tier: BackstopTier, pool: Address, amount: i128) -> i128 {
        require_nonnegative(&e, amount);
        preview_deposit(&storage::get_pool_balance_for_tier(&e, tier, &pool), amount)
    }

    fn preview_tier_withdrawal(e: Env, tier: BackstopTier, pool: Address, shares: i128) -> i128 {
        require_nonnegative(&e, shares);
        preview_withdrawal(&storage::get_pool_balance_for_tier(&e, tier, &pool), shares)
    }

    fn migration_state(e: Env) -> MigrationState {
        storage::extend_instance(&e);
        migration::state(&e)
    }

    fn drop(e: Env) {
        storage::extend_instance(&e);
        migration::drop(&e)
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

    /********** Emissions **********/

    fn distribute(e: Env) -> i128 {
        storage::extend_instance(&e);
        let distribution = emissions::distribute(&e);

        BackstopEvents::distribute(&e, distribution.distributed);
        distribution.distributed
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

    fn claim(
        e: Env,
        tier: BackstopTier,
        user: Address,
        pool: Address,
        min_lp_tokens_out: i128,
    ) -> i128 {
        storage::extend_instance(&e);
        let claim = emissions::claim_user_ongoing_blnd(&e, tier, &user, &pool, min_lp_tokens_out);
        BackstopEvents::claim(
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

    fn gulp_emissions(e: Env, pool: Address) -> i128 {
        storage::extend_instance(&e);
        let amount = emissions::gulp_pool_ongoing_emissions(&e, &pool);
        BackstopEvents::gulp_emissions(&e, pool, 0, amount);
        amount
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
