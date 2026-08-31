use crate::{
    backstop::{
        self, load_pool_backstop_data, tier_token, validate_backstop_assets, BackstopTier,
        PoolBackstopData, UserBalance, Q4W,
    },
    constants::{MAX_INITIAL_DROP, MAX_MIGRATION_DROP_LIST},
    dependencies::EmitterClient,
    emissions,
    errors::BackstopError,
    events::BackstopEvents,
    migration, storage,
};
use soroban_sdk::{contract, contractclient, contractimpl, panic_with_error, Address, Env, Vec};

/// ### Backstop
///
/// A backstop module for the BLNT protocol's isolated lending pools
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

    /// Queue every active share held by a depositor whose pool-specific
    /// backstop-deposit permission has been revoked.
    fn force_queue_withdrawal(
        e: Env,
        tier: BackstopTier,
        user: Address,
        pool_address: Address,
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

    /// Withdraw every currently matured queued share only to an unauthorized
    /// depositor's own address.
    fn force_withdrawal(e: Env, tier: BackstopTier, user: Address, pool_address: Address) -> i128;

    /// Fetch one user's active shares and bounded withdrawal queue in a tier.
    fn user_balance(e: Env, tier: BackstopTier, pool: Address, user: Address) -> UserBalance;

    /// Fetch the backstop data for the pool
    ///
    /// Return the pool's complete configured-tier accounting and valuation.
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool
    fn pool_data(e: Env, pool: Address) -> PoolBackstopData;

    /// Return the canonical Comet v2-implied seven-decimal BLNT price in USDC.
    fn blnt_price(e: Env) -> i128;

    /// Fetch the token contract bound to one pool's loss-waterfall position.
    fn backstop_token(e: Env, tier: BackstopTier, pool: Address) -> Address;

    /// Fetch the reward zone for the backstop
    fn reward_zone(e: Env) -> Vec<Address>;

    /********** Emissions **********/

    /// Allocate the next migration-backfill or ongoing BLNT checkpoint.
    fn distribute(e: Env) -> i128;

    /// Start or refresh the pool's tier streams and grant its accrued 30% allowance.
    fn gulp_emissions(e: Env, pool: Address) -> i128;

    /// Add a threshold-qualified pool to the reward zone.
    ///
    /// If all 30 slots are occupied, the replacement must have strictly more
    /// active underlying BLNT than the named member.
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
    /// ### Arguments
    /// * `to_remove` - The address of the pool to remove
    ///
    /// ### Errors
    /// If the pool is not below the threshold or if the pool is not in the reward zone
    fn remove_reward(e: Env, to_remove: Address);

    /// Compound one eligible tier's accrued BLNT across a list of pools.
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

    /// (Only Pool) Sends one tier token from `from` to a pool's backstop.
    /// Donations are credited in full to the selected backstop tier.
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
    /// * `blnt_usdc_token` - The canonical BLNT:USDC LP token
    /// * `blnt_xlm_token` - The canonical BLNT:XLM LP token
    /// * `emitter` - The Emitter contract ID
    /// * `blnt_token` - The BLNT token ID
    /// * `usdc_token` - The USDC token ID
    /// * `xlm_token` - The XLM token ID
    /// * `pool_factory` - The pool factory ID
    /// * `drop_list` - Immutable discretionary recipient addresses and BLNT amounts. The initial
    ///   emitter recipient may allocate up to 150 million BLNT; migration candidates may allocate
    ///   up to 40 million BLNT so the emitter's remaining allowance can fund migration backfill.
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        e: Env,
        blnt_usdc_token: Address,
        blnt_xlm_token: Address,
        emitter: Address,
        blnt_token: Address,
        usdc_token: Address,
        xlm_token: Address,
        pool_factory: Address,
        drop_list: Vec<(Address, i128)>,
    ) {
        validate_backstop_assets(
            &e,
            &blnt_token,
            &usdc_token,
            &xlm_token,
            &blnt_usdc_token,
            &blnt_xlm_token,
        );
        let mut drop_total = 0_i128;
        for (_, amount) in drop_list.iter() {
            require_nonnegative(&e, amount);
            drop_total = drop_total
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&e, BackstopError::OverflowError));
        }
        if drop_total > MAX_INITIAL_DROP {
            panic_with_error!(&e, BackstopError::BadRequest);
        }
        let extended_initial_drop = drop_total > MAX_MIGRATION_DROP_LIST;
        if extended_initial_drop
            && EmitterClient::new(&e, &emitter).get_backstop() != e.current_contract_address()
        {
            panic_with_error!(&e, BackstopError::BadRequest);
        }
        storage::set_blnt_usdc_token(&e, &blnt_usdc_token);
        storage::set_blnt_xlm_token(&e, &blnt_xlm_token);
        storage::set_blnt_token(&e, &blnt_token);
        storage::set_usdc_token(&e, &usdc_token);
        storage::set_xlm_token(&e, &xlm_token);
        storage::set_pool_factory(&e, &pool_factory);
        storage::set_emitter(&e, &emitter);
        storage::set_drop_list(&e, &drop_list);
        storage::set_extended_initial_drop(&e, extended_initial_drop);
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

        crate::access::require_deposit_permission(&e, &pool_address, &from);

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

    fn force_queue_withdrawal(
        e: Env,
        tier: BackstopTier,
        user: Address,
        pool_address: Address,
    ) -> Q4W {
        storage::extend_instance(&e);
        crate::access::require_deposit_permission_absent(&e, &pool_address, &user);

        let entry = backstop::execute_force_queue_withdrawal(&e, tier, &user, &pool_address);
        BackstopEvents::tier_queue_withdrawal(
            &e,
            tier,
            pool_address,
            user,
            entry.amount,
            entry.exp,
        );
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

        crate::access::require_deposit_permission(&e, &pool_address, &from);

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

    fn force_withdrawal(e: Env, tier: BackstopTier, user: Address, pool_address: Address) -> i128 {
        storage::extend_instance(&e);
        crate::access::require_deposit_permission_absent(&e, &pool_address, &user);

        let (shares, tokens) = backstop::execute_force_withdrawal(&e, tier, &user, &pool_address);
        BackstopEvents::tier_withdraw(&e, tier, pool_address, user, shares, tokens);
        tokens
    }

    fn user_balance(e: Env, tier: BackstopTier, pool: Address, user: Address) -> UserBalance {
        tier_token(&e, &pool, tier);
        storage::get_user_balance_for_tier(&e, tier, &pool, &user)
    }

    fn pool_data(e: Env, pool: Address) -> PoolBackstopData {
        load_pool_backstop_data(&e, &pool)
    }

    fn blnt_price(e: Env) -> i128 {
        backstop::quote_blnt_price(&e)
    }

    fn backstop_token(e: Env, tier: BackstopTier, pool: Address) -> Address {
        tier_token(&e, &pool, tier)
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
        for (pool, blnt_amount, lp_amount, shares) in claim.allocations.iter() {
            BackstopEvents::claim(&e, tier, from.clone(), pool, blnt_amount, lp_amount, shares);
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

#[cfg(test)]
mod tests {
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        Address, Env,
    };

    use crate::testutils::{
        create_backstop, create_backstop_token, create_mock_pool, create_mock_pool_factory,
        MockAccessController, MockAccessControllerClient,
    };

    use super::*;

    #[test]
    fn revoked_depositor_can_be_queued_and_withdrawn_only_to_self() {
        let e = Env::default();
        e.cost_estimate().budget().reset_unlimited();
        e.mock_all_auths_allowing_non_root_auth();
        e.ledger().set(LedgerInfo {
            timestamp: 10_000,
            protocol_version: 27,
            sequence_number: 200,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });

        let backstop = create_backstop(&e);
        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let (pool, _) = create_mock_pool(&e, &backstop);
        let (_, token) = create_backstop_token(&e, &backstop, &admin);
        token.mint(&user, &100_0000000);

        let controller = e.register(MockAccessController, ());
        let controller_client = MockAccessControllerClient::new(&e, &controller);
        let (_, factory) = create_mock_pool_factory(&e, &backstop);
        factory.set_pool(&pool);
        factory.set_pool_access_controller(&pool, &Some(controller));
        controller_client.set_permissions(&pool, &user, &crate::access::BACKSTOP_DEPOSIT_ALLOWED);

        let client = BackstopClient::new(&e, &backstop);
        assert_eq!(
            client.deposit(&BackstopTier::SecondLoss, &user, &pool, &100_0000000,),
            100_0000000
        );
        controller_client.set_permissions(&pool, &user, &0);
        let q4w = client.force_queue_withdrawal(&BackstopTier::SecondLoss, &user, &pool);
        assert_eq!(q4w.amount, 100_0000000);
        assert_eq!(
            client
                .user_balance(&BackstopTier::SecondLoss, &pool, &user)
                .shares,
            0
        );

        e.ledger().set(LedgerInfo {
            timestamp: q4w.exp,
            protocol_version: 27,
            sequence_number: 200 + 17 * 17_280,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 10,
            min_persistent_entry_ttl: 10,
            max_entry_ttl: 3_110_400,
        });
        assert_eq!(
            client.force_withdrawal(&BackstopTier::SecondLoss, &user, &pool),
            100_0000000
        );
        assert_eq!(token.balance(&user), 100_0000000);
        assert!(client
            .user_balance(&BackstopTier::SecondLoss, &pool, &user)
            .q4w
            .is_empty());
    }
}
