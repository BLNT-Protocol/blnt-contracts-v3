use crate::{
    auctions::{self, AuctionData},
    emissions::{self, ReserveEmissionMetadata},
    events::PoolEvents,
    pool::{self, FlashLoan, Positions, Request, Reserve},
    storage::{self, ReserveConfig},
    PoolConfig, PoolError, ReserveEmissionData, UserEmissionData,
};
use sep_41_token::StellarAssetClient;
use soroban_sdk::{
    contract, contractclient, contractimpl, panic_with_error, Address, Env, String, Vec,
};

/// ### Pool
///
/// An isolated money market pool.
#[contract]
pub struct PoolContract;

#[contractclient(name = "PoolClient")]
pub trait Pool {
    /// (Admin only) Set a new address to become the admin of the pool. This
    /// must be accepted by the new admin w/ `accept_admin` to take effect.
    ///
    /// ### Arguments
    /// * `new_admin` - The new admin address
    ///
    /// ### Panics
    /// If the caller is not the admin
    fn propose_admin(e: Env, new_admin: Address);

    /// (Proposed admin only) Accept the admin role. Ensures the new admin
    /// can safely submit transactions before taking over the pool admin role.
    ///
    /// ### Panics
    /// If the caller is not the proposed admin
    fn accept_admin(e: Env);

    /// (Admin only) Update the pool
    ///
    /// ### Arguments
    /// * `backstop_take_rate` - The new take rate for the backstop (7 decimals)
    /// * `max_positions` - The new maximum number of allowed positions for a single user's account
    /// * `min_collateral` - The new minimum collateral required to open a borrow position,
    ///                      in the oracles base asset decimals
    ///
    /// ### Panics
    /// If the caller is not the admin
    fn update_pool(e: Env, backstop_take_rate: u32, max_positions: u32, min_collateral: i128);

    /// (Admin only) Queues setting data for a reserve in the pool
    ///
    /// ### Arguments
    /// * `asset` - The underlying asset to add as a reserve
    /// * `config` - The ReserveConfig for the reserve
    ///
    /// ### Panics
    /// If the caller is not the admin
    fn queue_set_reserve(e: Env, asset: Address, metadata: ReserveConfig);

    /// (Admin only) Cancels the queued set of a reserve in the pool
    ///
    /// ### Arguments
    /// * `asset` - The underlying asset to add as a reserve
    ///
    /// ### Panics
    /// If the caller is not the admin or the reserve is not queued for initialization
    fn cancel_set_reserve(e: Env, asset: Address);

    /// Executes the queued set of a reserve in the pool
    ///
    /// ### Arguments
    /// * `asset` - The underlying asset to add as a reserve
    ///
    /// ### Panics
    /// If the reserve is not queued for initialization
    /// or is already setup
    /// or has invalid metadata
    fn set_reserve(e: Env, asset: Address) -> u32;

    /// Fetch the pool configuration
    fn get_config(e: Env) -> PoolConfig;

    /// Fetch the admin address of the pool
    fn get_admin(e: Env) -> Address;

    /// Fetch the a vec addresses of all reserves in the pool. The index of the reserve
    /// in this vec defines the index of the reserve in the pool, used in places like `Positions`.
    fn get_reserve_list(e: Env) -> Vec<Address>;

    /// Fetch information about a reserve, updated to the current ledger
    ///
    /// ### Arguments
    /// * `asset` - The address of the reserve asset
    fn get_reserve(e: Env, asset: Address) -> Reserve;

    /// Fetch the positions for an address. For each position type, there is a map of the reserve index
    /// to the position for that reserve, if it exists.
    ///
    /// ### Arguments
    /// * `address` - The address to fetch positions for
    fn get_positions(e: Env, address: Address) -> Positions;

    /// Submit a set of requests to the pool where `from` takes on the position, `spender` sends any
    /// required tokens to the pool and `to` receives any tokens sent from the pool.
    ///
    /// Returns the new positions for `from`
    ///
    /// ### Arguments
    /// * `from` - The address of the user whose positions are being modified
    /// * `spender` - The address of the user who is sending tokens to the pool
    /// * `to` - The address of the user who is receiving tokens from the pool
    /// * `requests` - A vec of requests to be processed
    ///
    /// ### Panics
    /// If the request is not able to be completed for cases like insufficient funds or invalid health factor
    fn submit(
        e: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<Request>,
    ) -> Positions;

    /// Submit a set of requests to the pool where `from` takes on the position, `spender` sends any
    /// required tokens to the pool using transfer_from and `to` receives any tokens sent from the pool.
    ///
    /// Returns the new positions for `from`
    ///
    /// ### Arguments
    /// * `from` - The address of the user whose positions are being modified
    /// * `spender` - The address of the user who is sending tokens to the pool
    /// * `to` - The address of the user who is receiving tokens from the pool
    /// * `requests` - A vec of requests to be processed
    ///
    /// ### Panics
    /// If the request is not able to be completed for cases like insufficient funds, insufficient allowance, or invalid health factor
    fn submit_with_allowance(
        e: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<Request>,
    ) -> Positions;

    /// Submit flash loan and a set of requests to the pool where `from` takes on the position. The flash loan will be invoked using
    /// the `flash_loan` arguments and `from` as the caller. For the requests, `from` sends any required tokens to the pool
    /// using transfer_from and receives any tokens sent from the pool.
    ///
    /// Returns the new positions for `from`
    ///
    /// ### Arguments
    /// * `from` - The address of the user whose positions are being modified and also the address of
    /// the user who is sending and receiving the tokens to the pool.
    /// * `flash_loan` - Arguments relative to the flash loan: receiver contract, asset and borroed amount.
    /// * `requests` - A vec of requests to be processed
    ///
    /// ### Panics
    /// If the request is not able to be completed for cases like insufficient funds ,insufficient allowance, or invalid health factor
    fn flash_loan(
        e: Env,
        from: Address,
        flash_loan: FlashLoan,
        requests: Vec<Request>,
    ) -> Positions;

    /// Claw back an exact amount of a user's supplied reserve.
    ///
    /// The reserve's Stellar Asset Contract requires its administrator's
    /// authorization and rejects the operation unless this pool's balance
    /// entry is clawbackable. Ordinary supply is removed before collateral.
    /// The operation may leave the user unhealthy for the inherited
    /// liquidation and bad-debt paths to resolve. If collateral is removed,
    /// an active user-liquidation auction is atomically invalidated.
    fn clawback(e: Env, asset: Address, from: Address, amount: i128);

    /// Recognize a direct reserve-custody deficit against the affected
    /// reserve's supplier rate and then its unpaid take-rate credit. The
    /// operation is permissionless, leaves user bToken balances and borrower
    /// liabilities unchanged, and creates no backstop debt.
    ///
    /// Returns the underlying deficit recognized, or zero when the reserve has
    /// no custody deficit.
    fn reconcile_loss(e: Env, asset: Address) -> i128;

    /// Update the pool status from canonical configured-tier USDC valuation and Q4W value.
    ///
    /// Backstop-triggered statuses are odd:
    /// * 1 = active
    /// * 3 = on-ice when the applicable activation threshold is not met, or
    ///   Q4W reaches the inherited 30% or 50% boundary
    /// * 5 = frozen when Q4W reaches the inherited 60% or 75% boundary
    ///
    /// ### Panics
    /// If the pool is currently on status 4, "admin-freeze", where only the admin
    /// can perform a status update via `set_status`
    fn update_status(e: Env) -> u32;

    /// (Admin only) Pool status is changed to `pool_status`
    /// * 0 = admin active - requires the applicable activation threshold and Q4W below 50%
    /// * 2 = admin on-ice - requires Q4W below 75%
    /// * 4 = admin frozen - can always be set
    ///
    /// ### Arguments
    /// * `pool_status` - The pool status to be set
    ///
    /// ### Panics
    /// If the caller is not the admin
    /// If the specified conditions are not met for the status to be set
    fn set_status(e: Env, pool_status: u32);

    /// Gulps unaccounted for tokens to the backstop credit so they aren't lost. This is most relevant
    /// for rebasing tokens where the token balance of the pool can increase without any corresponding
    /// transfer.
    ///
    /// Blend Pools do not support fee-on-transaction tokens, or any tokens in which the pools balance
    /// can decrease without any corresponding withdraw. Thus, negative token deltas are ignored.
    ///
    /// ### Arguments
    /// * `asset` - The address of the asset to gulp
    ///
    /// Returns the amount of tokens gulped
    fn gulp(e: Env, asset: Address) -> i128;

    /********* Emission Functions **********/

    /// Consume emissions from the backstop and distribute to the reserves based
    /// on the reserve emission configuration.
    ///
    /// Returns amount of new tokens emitted
    fn gulp_emissions(e: Env) -> i128;

    /// (Admin only) Set the emission configuration for the pool
    ///
    /// Changes will be applied in the next pool `update_emissions`, and affect the next emission cycle
    ///
    /// ### Arguments
    /// * `res_emission_metadata` - A vector of ReserveEmissionMetadata to update metadata to
    ///
    /// ### Panics
    /// * If the caller is not the admin
    fn set_emissions_config(e: Env, res_emission_metadata: Vec<ReserveEmissionMetadata>);

    /// Claims outstanding emissions for the caller for the given reserve's.
    ///
    /// A reserve token id is a unique identifier for a position in a pool.
    /// - For a reserve's dTokens (liabilities), reserve_token_id = reserve_index * 2
    /// - For a reserve's bTokens (supply/collateral), reserve_token_id = reserve_index * 2 + 1
    ///
    /// Returns the number of tokens claimed
    ///
    /// ### Arguments
    /// * `from` - The address claiming
    /// * `reserve_token_ids` - Vector of reserve token ids
    /// * `to` - The Address to send the claimed tokens to
    fn claim(e: Env, from: Address, reserve_token_ids: Vec<u32>, to: Address) -> i128;

    /// Get the emissions data for a reserve token
    ///
    /// A reserve token id is a unique identifier for a position in a pool.
    /// - For a reserve's dTokens (liabilities), reserve_token_id = reserve_index * 2
    /// - For a reserve's bTokens (supply/collateral), reserve_token_id = reserve_index * 2 + 1
    ///
    /// ### Arguments
    /// * `reserve_token_id` - The reserve token id
    fn get_reserve_emissions(e: Env, reserve_token_id: u32) -> Option<ReserveEmissionData>;

    /// Get the emissions data for a user
    ///
    /// A reserve token id is a unique identifier for a position in a pool.
    /// - For a reserve's dTokens (liabilities), reserve_token_id = reserve_index * 2
    /// - For a reserve's bTokens (supply/collateral), reserve_token_id = reserve_index * 2 + 1
    ///
    /// ### Arguments
    /// * `user` - The address of the user
    /// * `reserve_token_id` - The reserve token id
    fn get_user_emissions(e: Env, user: Address, reserve_token_id: u32)
        -> Option<UserEmissionData>;

    /***** Auction / Liquidation Functions *****/

    /// Create a liquidation, bad-debt, or interest auction.
    ///
    /// `bid` and `lot` have these auction-specific meanings:
    /// - Liquidation: both are required auction inputs; neither may be empty.
    /// - Bad debt: either may be `[]`, meaning the caller accepts the corresponding canonical
    ///   asset set. A nonempty `bid` must exactly match the canonical debt assets, and a nonempty
    ///   `lot` must exactly match the selected waterfall-tier token.
    /// - Interest: `bid` may be `[]`, meaning the caller accepts the canonically selected tier
    ///   token, or nonempty to assert that exact token. `lot` is the required, nonempty list of
    ///   reserve assets to checkpoint and offer; `lot = []` is invalid.
    ///
    /// Assertions are order-independent and never choose a tier, set an amount, or affect pricing.
    /// A mismatch fails the transaction atomically.
    ///
    /// ### Arguments
    /// * `auction_type` - 0 for liquidation, 1 for bad debt, or 2 for interest
    /// * `user` - The borrower for liquidation or the configured backstop otherwise
    /// * `bid` - Bid assets or optional backstop-auction asset assertion, as described above
    /// * `lot` - Lot assets, optional bad-debt asset assertion, or interest reserve inputs
    /// * `percent` - Liquidation percentage; exactly 100 otherwise
    ///
    /// A nonempty assertion that does not exactly match the canonical asset set fails with
    /// `InvalidBid` or `InvalidLot`, as applicable.
    fn new_auction(
        e: Env,
        auction_type: u32,
        user: Address,
        bid: Vec<Address>,
        lot: Vec<Address>,
        percent: u32,
    ) -> AuctionData;

    /// Return an auction's v2-compatible base bid, lot, and starting ledger.
    fn get_auction(e: Env, auction_type: u32, user: Address) -> AuctionData;

    /// Delete a stale auction after the inherited 500-ledger boundary.
    fn del_auction(e: Env, auction_type: u32, user: Address);

    /// Check and handle bad debt for a user.
    ///
    /// * If the user is not the backstop and they have bad debt, the backstop will take over the debt.
    /// * If the user is the backstop, residual debt defaults to suppliers only
    ///   after all configured tiers are verified to have no usable value.
    ///
    /// ### Arguments
    /// * `user` - The address of the user to check for bad debt
    ///
    /// ### Panics
    /// * If there is no bad debt to handle
    /// * If there is an ongoing auction for the user
    fn bad_debt(e: Env, user: Address);
}

#[contractimpl]
impl PoolContract {
    /// Initialize the pool
    ///
    /// ### Arguments
    /// Creator supplied:
    /// * `admin` - The Address for the admin
    /// * `name` - The name of the pool
    /// * `oracle` - The contract address of the oracle
    /// * `backstop_take_rate` - The take rate for the backstop (7 decimals)
    /// * `max_positions` - The maximum number of positions a user is permitted to have
    /// * `min_collateral` - The minimum collateral required to open a borrow position in the oracles base asset
    ///
    /// Pool Factory supplied:
    /// * `backstop_id` - The contract address of the pool's backstop module
    /// * `blnd_id` - The contract ID of the BLND token
    pub fn __constructor(
        e: Env,
        admin: Address,
        name: String,
        oracle: Address,
        bstop_rate: u32,
        max_positions: u32,
        min_collateral: i128,
        backstop_id: Address,
        blnd_id: Address,
    ) {
        admin.require_auth();

        pool::execute_initialize(
            &e,
            &admin,
            &name,
            &oracle,
            &bstop_rate,
            &max_positions,
            &min_collateral,
            &backstop_id,
            &blnd_id,
        );
    }
}

#[contractimpl]
impl Pool for PoolContract {
    fn propose_admin(e: Env, new_admin: Address) {
        storage::extend_instance(&e);
        let admin = storage::get_admin(&e);
        admin.require_auth();

        storage::set_proposed_admin(&e, &new_admin);
    }

    fn accept_admin(e: Env) {
        storage::extend_instance(&e);

        if let Some(proposed_admin) = storage::get_proposed_admin(&e) {
            proposed_admin.require_auth();
            let cur_admin = storage::get_admin(&e);

            storage::set_admin(&e, &proposed_admin);

            PoolEvents::set_admin(&e, cur_admin, proposed_admin);
        } else {
            panic_with_error!(&e, PoolError::BadRequest);
        }
    }

    fn update_pool(e: Env, backstop_take_rate: u32, max_positions: u32, min_collateral: i128) {
        storage::extend_instance(&e);
        let admin = storage::get_admin(&e);
        admin.require_auth();

        pool::execute_update_pool(&e, backstop_take_rate, max_positions, min_collateral);

        PoolEvents::update_pool(&e, admin, backstop_take_rate, max_positions, min_collateral);
    }

    fn queue_set_reserve(e: Env, asset: Address, metadata: ReserveConfig) {
        storage::extend_instance(&e);
        let admin = storage::get_admin(&e);
        admin.require_auth();

        pool::execute_queue_set_reserve(&e, &asset, &metadata);

        PoolEvents::queue_set_reserve(&e, admin, asset, metadata);
    }

    fn cancel_set_reserve(e: Env, asset: Address) {
        storage::extend_instance(&e);
        let admin = storage::get_admin(&e);
        admin.require_auth();

        pool::execute_cancel_queued_set_reserve(&e, &asset);

        PoolEvents::cancel_set_reserve(&e, admin, asset);
    }

    fn set_reserve(e: Env, asset: Address) -> u32 {
        storage::extend_instance(&e);

        let index = pool::execute_set_reserve(&e, &asset);

        PoolEvents::set_reserve(&e, asset, index);
        index
    }

    fn get_config(e: Env) -> PoolConfig {
        storage::get_pool_config(&e)
    }

    fn get_admin(e: Env) -> Address {
        storage::get_admin(&e)
    }

    fn get_reserve_list(e: Env) -> Vec<Address> {
        storage::get_res_list(&e)
    }

    fn get_reserve(e: Env, asset: Address) -> Reserve {
        let pool_config = storage::get_pool_config(&e);
        Reserve::load(&e, &pool_config, &asset)
    }

    fn get_positions(e: Env, address: Address) -> Positions {
        storage::get_user_positions(&e, &address)
    }

    fn submit(
        e: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<Request>,
    ) -> Positions {
        storage::extend_instance(&e);
        spender.require_auth();
        if from != spender {
            from.require_auth();
        }

        pool::execute_submit(&e, &from, &spender, &to, requests, false)
    }

    fn submit_with_allowance(
        e: Env,
        from: Address,
        spender: Address,
        to: Address,
        requests: Vec<Request>,
    ) -> Positions {
        storage::extend_instance(&e);
        spender.require_auth();
        if from != spender {
            from.require_auth();
        }

        pool::execute_submit(&e, &from, &spender, &to, requests, true)
    }

    fn flash_loan(
        e: Env,
        from: Address,
        flash_loan: FlashLoan,
        requests: Vec<Request>,
    ) -> Positions {
        storage::extend_instance(&e);
        from.require_auth();

        pool::execute_submit_with_flash_loan(&e, &from, flash_loan, requests)
    }

    fn clawback(e: Env, asset: Address, from: Address, amount: i128) {
        storage::extend_instance(&e);
        StellarAssetClient::new(&e, &asset).admin().require_auth();

        let (supply_burned, collateral_burned, auction_invalidated) =
            pool::execute_clawback(&e, &asset, &from, amount);

        if auction_invalidated {
            PoolEvents::delete_auction(
                &e,
                auctions::AuctionType::UserLiquidation as u32,
                from.clone(),
            );
        }

        PoolEvents::clawback(&e, asset, from, amount, supply_burned, collateral_burned);
    }

    fn reconcile_loss(e: Env, asset: Address) -> i128 {
        storage::extend_instance(&e);
        let (loss, supplier_loss, backstop_credit_loss, b_rate_loss, auction_canceled) =
            pool::execute_reconcile_loss(&e, &asset);

        if auction_canceled {
            PoolEvents::delete_auction(
                &e,
                auctions::AuctionType::InterestAuction as u32,
                storage::get_backstop(&e),
            );
        }
        PoolEvents::reconcile_loss(
            &e,
            asset,
            loss,
            supplier_loss,
            backstop_credit_loss,
            b_rate_loss,
        );
        loss
    }

    fn update_status(e: Env) -> u32 {
        storage::extend_instance(&e);
        let new_status = pool::execute_update_pool_status(&e);

        PoolEvents::set_status(&e, new_status);
        new_status
    }

    fn set_status(e: Env, pool_status: u32) {
        storage::extend_instance(&e);
        let admin = storage::get_admin(&e);
        admin.require_auth();
        pool::execute_set_pool_status(&e, pool_status);

        PoolEvents::set_status_admin(&e, admin, pool_status);
    }

    fn gulp(e: Env, asset: Address) -> i128 {
        storage::extend_instance(&e);
        let token_delta = pool::execute_gulp(&e, &asset);

        PoolEvents::gulp(&e, asset, token_delta);
        token_delta
    }

    /********* Emission Functions **********/

    fn gulp_emissions(e: Env) -> i128 {
        storage::extend_instance(&e);
        let emissions = emissions::gulp_emissions(&e);

        PoolEvents::gulp_emissions(&e, emissions);
        emissions
    }

    fn set_emissions_config(e: Env, res_emission_metadata: Vec<ReserveEmissionMetadata>) {
        storage::extend_instance(&e);
        let admin = storage::get_admin(&e);
        admin.require_auth();

        emissions::set_pool_emissions(&e, res_emission_metadata);
    }

    fn claim(e: Env, from: Address, reserve_token_ids: Vec<u32>, to: Address) -> i128 {
        storage::extend_instance(&e);
        from.require_auth();

        let amount_claimed = emissions::execute_claim(&e, &from, &reserve_token_ids, &to);

        PoolEvents::claim(&e, from, reserve_token_ids, amount_claimed);

        amount_claimed
    }

    fn get_reserve_emissions(e: Env, reserve_token_index: u32) -> Option<ReserveEmissionData> {
        storage::get_res_emis_data(&e, &reserve_token_index)
    }

    fn get_user_emissions(
        e: Env,
        user: Address,
        reserve_token_index: u32,
    ) -> Option<UserEmissionData> {
        storage::get_user_emissions(&e, &user, &reserve_token_index)
    }

    /***** Auction / Liquidation Functions *****/

    fn new_auction(
        e: Env,
        auction_type: u32,
        user: Address,
        bid: Vec<Address>,
        lot: Vec<Address>,
        percent: u32,
    ) -> AuctionData {
        storage::extend_instance(&e);

        let auction_data = auctions::create_auction(&e, auction_type, &user, &bid, &lot, percent);

        PoolEvents::new_auction(&e, auction_type, user, percent, auction_data.clone());
        auction_data
    }

    fn get_auction(e: Env, auction_type: u32, user: Address) -> AuctionData {
        auctions::get_auction(&e, auction_type, &user)
    }

    fn del_auction(e: Env, auction_type: u32, user: Address) {
        storage::extend_instance(&e);

        auctions::delete_stale_auction(&e, auction_type, &user);
        PoolEvents::delete_auction(&e, auction_type, user);
    }

    fn bad_debt(e: Env, user: Address) {
        storage::extend_instance(&e);

        pool::bad_debt(&e, &user);
    }
}
