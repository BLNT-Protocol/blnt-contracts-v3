use crate::{
    backstop::{
        self, load_pool_backstop_data, load_pool_tier_state, preview_deposit, preview_withdrawal,
        tier_token, user_queued_shares, user_total_shares, BackstopTier, PoolBackstopData,
        PoolTierState, TierTotals, Q4W,
    },
    constants::{MAX_BACKFILLED_EMISSIONS, SCALAR_7},
    dependencies::{EmitterClient, PoolFactoryClient},
    emissions,
    errors::BackstopError,
    events::BackstopEvents,
    storage,
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

    /// Deposit into the BLND:USDC first-loss tier.
    fn deposit_blnd_usdc(e: Env, from: Address, pool_address: Address, amount: i128) -> i128;

    /// Deposit BLND:XLM LP into the second-loss tier.
    fn deposit_blnd_xlm(e: Env, from: Address, pool_address: Address, amount: i128) -> i128;

    /// Deposit plain USDC into the third-loss tier.
    fn deposit_usdc(e: Env, from: Address, pool_address: Address, amount: i128) -> i128;

    fn queue_blnd_usdc_withdrawal(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> Q4W;

    fn queue_blnd_xlm_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128)
        -> Q4W;

    fn queue_usdc_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128) -> Q4W;

    fn dequeue_blnd_usdc_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128);

    fn dequeue_blnd_xlm_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128);

    fn dequeue_usdc_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128);

    fn withdraw_blnd_usdc(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128;

    fn withdraw_blnd_xlm(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128;

    fn withdraw_usdc(
        e: Env,
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

    /// Fetch the reward zone for the backstop
    fn reward_zone(e: Env) -> Vec<Address>;

    /********** Emissions **********/

    /// Update the backstop with new emissions for all reward zone pools
    ///
    /// Returns the amount of new emissions for all reward zone pools
    fn distribute(e: Env) -> i128;

    /// Distribute emissions to a reward zone pool and its backstop
    ///
    /// Returns the amount of BLND emissions distributed to the pool
    ///
    /// ### Arguments
    /// * `pool` - The address of the pool to distribute emissions to
    ///
    /// ### Errors
    /// If the pool is not in the reward zone or the pool does not authorize the call
    fn gulp_emissions(e: Env, pool: Address) -> i128;

    /// Add a pool to the reward zone, and if the reward zone is full, a pool to remove
    ///
    /// ### Arguments
    /// * `to_add` - The address of the pool to add
    /// * `to_remove` - The address of the pool to remove (Optional - Used if the reward zone is full)
    ///
    /// ### Errors
    /// If the pool to remove has more tokens, or if distribute has not occured in the last hour
    fn add_reward(e: Env, to_add: Address, to_remove: Option<Address>);

    /// Remove a pool from the reward zone
    ///
    /// ### Arguments
    /// * `to_remove` - The address of the pool to remove
    ///
    /// ### Errors
    /// If the pool is not below the threshold or if the pool is not in the reward zone
    fn remove_reward(e: Env, to_remove: Address);

    /// Claim backstop deposit emissions from a list of pools for `from`
    ///
    /// Returns the amount of LP tokens minted
    ///
    /// ### Arguments
    /// * `from` - The address of the user claiming emissions
    /// * `pool_addresses` - The Vec of addresses to claim backstop deposit emissions from
    /// * `min_lp_tokens_out` - The minimum amount of LP tokens to mint with the claimed BLND
    ///
    /// ### Errors
    /// If an invalid pool address is included
    fn claim(e: Env, from: Address, pool_addresses: Vec<Address>, min_lp_tokens_out: i128) -> i128;

    /// Drop initial BLND to a list of addresses through the emitter
    fn drop(e: Env);

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
    /// * `blnd_usdc_token` - The first-tier LP token with the pair BLND:USDC
    /// * `blnd_xlm_token` - The second-tier LP token with the pair BLND:XLM
    /// * `emitter` - The Emitter contract ID
    /// * `blnd_token` - The BLND token ID
    /// * `usdc_token` - The USDC token ID
    /// * `pool_factory` - The pool factory ID
    /// * `drop_list` - The list of addresses to distribute initial BLND to and the percent of the distribution they should receive
    pub fn __constructor(
        e: Env,
        blnd_usdc_token: Address,
        blnd_xlm_token: Address,
        emitter: Address,
        blnd_token: Address,
        usdc_token: Address,
        pool_factory: Address,
        drop_list: Vec<(Address, i128)>,
    ) {
        if blnd_usdc_token == blnd_xlm_token
            || blnd_usdc_token == usdc_token
            || blnd_xlm_token == usdc_token
        {
            panic_with_error!(&e, BackstopError::AssetConfigurationCollision);
        }
        let factory = PoolFactoryClient::new(&e, &pool_factory);
        if factory.backstop() != e.current_contract_address() {
            panic_with_error!(&e, BackstopError::InvalidPoolFactoryBinding);
        }
        let _ = factory.is_pool(&e.current_contract_address());

        storage::set_blnd_usdc_token(&e, &blnd_usdc_token);
        storage::set_blnd_xlm_token(&e, &blnd_xlm_token);
        storage::set_blnd_token(&e, &blnd_token);
        storage::set_usdc_token(&e, &usdc_token);
        storage::set_pool_factory(&e, &pool_factory);
        let mut drop_total: i128 = 0;
        for (_, amount) in drop_list.iter() {
            drop_total += amount;
        }
        if drop_total + MAX_BACKFILLED_EMISSIONS > 50_000_000 * SCALAR_7 {
            panic_with_error!(&e, BackstopError::BadRequest);
        }
        storage::set_drop_list(&e, &drop_list);
        storage::set_emitter(&e, &emitter);
    }
}

/// @dev
/// The contract implementation only manages the authorization / authentication required from the caller(s), and
/// utilizes other modules to carry out contract functionality.
#[contractimpl]
impl Backstop for BackstopContract {
    /********** Core **********/

    fn deposit_blnd_usdc(e: Env, from: Address, pool_address: Address, amount: i128) -> i128 {
        deposit_tier(e, BackstopTier::BlndUsdc, from, pool_address, amount)
    }

    fn deposit_blnd_xlm(e: Env, from: Address, pool_address: Address, amount: i128) -> i128 {
        deposit_tier(e, BackstopTier::BlndXlm, from, pool_address, amount)
    }

    fn deposit_usdc(e: Env, from: Address, pool_address: Address, amount: i128) -> i128 {
        deposit_tier(e, BackstopTier::Usdc, from, pool_address, amount)
    }

    fn queue_blnd_usdc_withdrawal(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> Q4W {
        queue_tier_withdrawal(e, BackstopTier::BlndUsdc, from, pool_address, amount)
    }

    fn queue_blnd_xlm_withdrawal(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
    ) -> Q4W {
        queue_tier_withdrawal(e, BackstopTier::BlndXlm, from, pool_address, amount)
    }

    fn queue_usdc_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128) -> Q4W {
        queue_tier_withdrawal(e, BackstopTier::Usdc, from, pool_address, amount)
    }

    fn dequeue_blnd_usdc_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128) {
        dequeue_tier_withdrawal(e, BackstopTier::BlndUsdc, from, pool_address, amount)
    }

    fn dequeue_blnd_xlm_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128) {
        dequeue_tier_withdrawal(e, BackstopTier::BlndXlm, from, pool_address, amount)
    }

    fn dequeue_usdc_withdrawal(e: Env, from: Address, pool_address: Address, amount: i128) {
        dequeue_tier_withdrawal(e, BackstopTier::Usdc, from, pool_address, amount)
    }

    fn withdraw_blnd_usdc(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128 {
        withdraw_tier(e, BackstopTier::BlndUsdc, from, pool_address, amount, to)
    }

    fn withdraw_blnd_xlm(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128 {
        withdraw_tier(e, BackstopTier::BlndXlm, from, pool_address, amount, to)
    }

    fn withdraw_usdc(
        e: Env,
        from: Address,
        pool_address: Address,
        amount: i128,
        to: Address,
    ) -> i128 {
        withdraw_tier(e, BackstopTier::Usdc, from, pool_address, amount, to)
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

    fn reward_zone(e: Env) -> Vec<Address> {
        storage::get_reward_zone(&e)
    }

    /********** Emissions **********/

    fn distribute(e: Env) -> i128 {
        storage::extend_instance(&e);
        let new_emissions = emissions::distribute(&e);

        BackstopEvents::distribute(&e, new_emissions);
        new_emissions
    }

    fn gulp_emissions(e: Env, pool: Address) -> i128 {
        storage::extend_instance(&e);
        pool.require_auth();
        let (backstop_emissions, pool_emissions) = emissions::gulp_emissions(&e, &pool);

        BackstopEvents::gulp_emissions(&e, pool, backstop_emissions, pool_emissions);
        pool_emissions
    }

    fn add_reward(e: Env, to_add: Address, to_remove: Option<Address>) {
        storage::extend_instance(&e);
        emissions::add_to_reward_zone(&e, to_add.clone(), to_remove.clone());

        BackstopEvents::rw_zone_add(&e, to_add, to_remove);
    }

    fn remove_reward(e: Env, to_remove: Address) {
        storage::extend_instance(&e);
        emissions::remove_from_reward_zone(&e, to_remove.clone());

        BackstopEvents::rw_zone_remove(&e, to_remove);
    }

    fn claim(e: Env, from: Address, pool_addresses: Vec<Address>, min_lp_tokens_out: i128) -> i128 {
        storage::extend_instance(&e);
        from.require_auth();

        let amount = emissions::execute_claim(&e, &from, &pool_addresses, &min_lp_tokens_out);

        BackstopEvents::claim(&e, from, amount);
        amount
    }

    fn drop(e: Env) {
        let mut drop_list = storage::get_drop_list(&e);
        let backfilled_emissions = storage::get_backfill_emissions(&e);
        drop_list.push_back((e.current_contract_address(), backfilled_emissions));
        let emitter_client = EmitterClient::new(&e, &storage::get_emitter(&e));
        emitter_client.drop(&drop_list)
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
