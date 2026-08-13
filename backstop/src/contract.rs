use crate::{
    backstop::{
        self, load_pool_backstop_data, tier_token, validate_backstop_assets, BackstopTier,
        PoolBackstopData, UserBalance, Q4W,
    },
    constants::{MAX_BACKFILLED_EMISSIONS, MAX_INITIAL_DROP},
    emissions,
    errors::BackstopError,
    events::BackstopEvents,
    migration, storage,
};
use soroban_sdk::{contract, contractclient, contractimpl, panic_with_error, Address, Env, Vec};

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

    /// Fetch one user's active shares and bounded withdrawal queue in a tier.
    fn user_balance(e: Env, tier: BackstopTier, pool: Address, user: Address) -> UserBalance;

    /// Fetch the backstop data for the pool
    ///
    /// Return the pool's complete three-tier accounting and valuation.
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool
    fn pool_data(e: Env, pool: Address) -> PoolBackstopData;

    /// Fetch the token contract bound to one fixed backstop tier.
    fn backstop_token(e: Env, tier: BackstopTier) -> Address;

    /// Fetch the reward zone for the backstop
    fn reward_zone(e: Env) -> Vec<Address>;

    /********** Emissions **********/

    /// Allocate the next migration-backfill or ongoing BLND checkpoint.
    fn distribute(e: Env) -> i128;

    /// Start or refresh the pool's tier streams and grant its accrued 30% allowance.
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

    /// Remove a pool below the activation threshold from the reward zone.
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

    /// Compound one eligible tier's accrued BLND across a list of pools.
    fn claim(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_addresses: Vec<Address>,
        min_lp_tokens_out: i128,
    ) -> i128;

    /// Execute the configured initial drop and fund any scheduled migration backfill.
    fn drop(e: Env);

    /********** Fund Management *********/

    /// (Only Pool) Take one tier token from a pool's backstop
    ///
    /// ### Arguments
    /// * `tier` - The tier whose token is drawn
    /// * `pool_address` - The address of the pool
    /// * `amount` - The amount of backstop tokens to draw
    /// * `to` - The address to send the backstop tokens to
    ///
    /// ### Errors
    /// If the pool does not have enough backstop tokens, or if the pool does
    /// not authorize the call
    fn draw(e: Env, tier: BackstopTier, pool_address: Address, amount: i128, to: Address);

    /// (Only Pool) Sends one tier token from `from` to a pool's backstop
    ///
    /// NOTE: This is not a deposit, and `from` will permanently lose access to the funds
    ///
    /// ### Arguments
    /// * `tier` - The tier whose token is donated
    /// * `from` - The address donating tokens to the backstop
    /// * `pool_address` - The address of the pool
    /// * `amount` - The amount of tier tokens to add
    ///
    /// ### Errors
    /// If the `pool_address` is not valid, backstop does not have sufficient allowance from `from`, or if the pool does not
    /// authorize the call
    fn donate(e: Env, tier: BackstopTier, from: Address, pool_address: Address, amount: i128);
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
    /// * `drop_list` - Immutable discretionary recipient addresses and BLND amounts
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
        drop_list: Vec<(Address, i128)>,
    ) {
        validate_backstop_assets(
            &e,
            &blnd_token,
            &usdc_token,
            &xlm_token,
            &blnd_usdc_token,
            &blnd_xlm_token,
        );
        let mut drop_total = MAX_BACKFILLED_EMISSIONS;
        for (_, amount) in drop_list.iter() {
            require_nonnegative(&e, amount);
            drop_total = drop_total
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&e, BackstopError::OverflowError));
        }
        if drop_total > MAX_INITIAL_DROP {
            panic_with_error!(&e, BackstopError::BadRequest);
        }
        storage::set_blnd_usdc_token(&e, &blnd_usdc_token);
        storage::set_blnd_xlm_token(&e, &blnd_xlm_token);
        storage::set_blnd_token(&e, &blnd_token);
        storage::set_usdc_token(&e, &usdc_token);
        storage::set_xlm_token(&e, &xlm_token);
        storage::set_pool_factory(&e, &pool_factory);
        storage::set_emitter(&e, &emitter);
        storage::set_drop_list(&e, &drop_list);
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
        storage::extend_instance(&e);
        from.require_auth();

        let shares = backstop::execute_deposit(&e, tier, &from, &pool_address, amount);

        BackstopEvents::tier_deposit(&e, tier, pool_address, from, amount, shares);
        shares
    }

    fn queue_withdrawal(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> Q4W {
        storage::extend_instance(&e);
        from.require_auth();

        let entry = backstop::execute_queue_withdrawal(&e, tier, &from, &pool_address, amount);

        BackstopEvents::tier_queue_withdrawal(&e, tier, pool_address, from, amount, entry.exp);
        entry
    }

    fn dequeue_withdrawal(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) {
        storage::extend_instance(&e);
        from.require_auth();

        backstop::execute_dequeue_withdrawal(&e, tier, &from, &pool_address, amount);

        BackstopEvents::tier_dequeue_withdrawal(&e, tier, pool_address, from, amount);
    }

    fn withdraw(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128 {
        storage::extend_instance(&e);
        from.require_auth();

        let tokens = backstop::execute_withdraw(&e, tier, &from, &pool_address, amount, &to);

        BackstopEvents::tier_withdraw(&e, tier, pool_address, from, amount, tokens);
        tokens
    }

    fn user_balance(e: Env, tier: BackstopTier, pool: Address, user: Address) -> UserBalance {
        storage::get_user_balance_for_tier(&e, tier, &pool, &user)
    }

    fn pool_data(e: Env, pool: Address) -> PoolBackstopData {
        load_pool_backstop_data(&e, &pool)
    }

    fn backstop_token(e: Env, tier: BackstopTier) -> Address {
        tier_token(&e, tier)
    }

    fn reward_zone(e: Env) -> Vec<Address> {
        storage::extend_instance(&e);
        emissions::get_reward_zone(&e)
    }

    /********** Emissions **********/

    fn distribute(e: Env) -> i128 {
        storage::extend_instance(&e);
        let distributed = emissions::distribute(&e);

        BackstopEvents::distribute(&e, distributed);
        distributed
    }

    fn gulp_emissions(e: Env, pool: Address) -> i128 {
        storage::extend_instance(&e);
        let (backstop_emissions, pool_emissions) = emissions::gulp_emissions(&e, &pool);
        BackstopEvents::gulp_emissions(&e, pool, backstop_emissions, pool_emissions);
        pool_emissions
    }

    fn add_reward(e: Env, to_add: Address, to_remove: Option<Address>) {
        storage::extend_instance(&e);
        let removed = emissions::add_to_reward_zone(&e, to_add.clone(), to_remove);

        BackstopEvents::rw_zone_add(&e, to_add, removed);
    }

    fn remove_reward(e: Env, to_remove: Address) {
        storage::extend_instance(&e);
        emissions::remove_from_reward_zone(&e, to_remove.clone());

        BackstopEvents::rw_zone_remove(&e, to_remove);
    }

    fn claim(
        e: Env,
        tier: BackstopTier,
        from: Address,
        pool_addresses: Vec<Address>,
        min_lp_tokens_out: i128,
    ) -> i128 {
        storage::extend_instance(&e);
        let claim = emissions::execute_claim(&e, tier, &from, &pool_addresses, min_lp_tokens_out);
        for (pool, blnd_amount, lp_amount, shares) in claim.allocations.iter() {
            BackstopEvents::claim(&e, tier, from.clone(), pool, blnd_amount, lp_amount, shares);
        }
        claim.lp_amount
    }

    fn drop(e: Env) {
        storage::extend_instance(&e);
        migration::drop(&e)
    }

    /********** Fund Management *********/

    fn draw(e: Env, tier: BackstopTier, pool_address: Address, amount: i128, to: Address) {
        storage::extend_instance(&e);
        pool_address.require_auth();

        backstop::execute_draw(&e, tier, &pool_address, amount, &to);

        BackstopEvents::draw(&e, tier, pool_address, to, amount);
    }

    fn donate(e: Env, tier: BackstopTier, from: Address, pool_address: Address, amount: i128) {
        storage::extend_instance(&e);
        from.require_auth();
        pool_address.require_auth();

        backstop::execute_donate(&e, tier, &from, &pool_address, amount);

        BackstopEvents::donate(&e, tier, pool_address, from, amount);
    }
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
