use soroban_sdk::{Address, BytesN, Env, Symbol};

use crate::backstop::{BackstopTier, BadDebtLotQuote};

pub struct BackstopEvents {}

impl BackstopEvents {
    pub fn bad_debt_lot_committed(
        e: &Env,
        pool: Address,
        auction_id: BytesN<32>,
        quote: BadDebtLotQuote,
    ) {
        let topics = (Symbol::new(e, "bad_debt_lot_committed"), pool);
        e.events().publish(topics, (auction_id, quote));
    }

    pub fn bad_debt_lot_released(e: &Env, pool: Address, auction_id: BytesN<32>) {
        let topics = (Symbol::new(e, "bad_debt_lot_released"), pool);
        e.events().publish(topics, auction_id);
    }

    /// Emit a v2-shaped deposit event scoped to a v3 tier.
    pub fn tier_deposit(
        e: &Env,
        tier: BackstopTier,
        pool_address: Address,
        from: Address,
        tokens_in: i128,
        shares_minted: i128,
    ) {
        let topics = (Symbol::new(e, "deposit"), tier, pool_address, from);
        e.events().publish(topics, (tokens_in, shares_minted));
    }

    /// Emit a v2-shaped queue event scoped to a v3 tier.
    pub fn tier_queue_withdrawal(
        e: &Env,
        tier: BackstopTier,
        pool_address: Address,
        from: Address,
        amount: i128,
        expiration: u64,
    ) {
        let topics = (Symbol::new(e, "queue_withdrawal"), tier, pool_address, from);
        e.events().publish(topics, (amount, expiration));
    }

    /// Emit a v2-shaped dequeue event scoped to a v3 tier.
    pub fn tier_dequeue_withdrawal(
        e: &Env,
        tier: BackstopTier,
        pool_address: Address,
        from: Address,
        amount: i128,
    ) {
        let topics = (
            Symbol::new(e, "dequeue_withdrawal"),
            tier,
            pool_address,
            from,
        );
        e.events().publish(topics, amount);
    }

    /// Emit a v2-shaped withdrawal event scoped to a v3 tier.
    pub fn tier_withdraw(
        e: &Env,
        tier: BackstopTier,
        pool_address: Address,
        from: Address,
        amount: i128,
        tokens_out: i128,
    ) {
        let topics = (Symbol::new(e, "withdraw"), tier, pool_address, from);
        e.events().publish(topics, (amount, tokens_out));
    }

    /// Emitted when new emissions are distributed
    /// - topics - `["distribute"]`
    /// - data - `[new_tokens_emitted: i128]`
    ///
    /// ### Arguments
    /// * `new_tokens_emitted` - The amount of new tokens emitted
    pub fn distribute(e: &Env, new_tokens_emitted: i128) {
        let topics = (Symbol::new(e, "distribute"),);
        e.events().publish(topics, new_tokens_emitted);
    }

    /// Emitted when new emissions are gulped
    ///
    /// - topics - `["gulp_emissions", pool_address: Address]`
    /// - data - `[new_backstop_emissions: i128, new_pool_emissions: i128]`
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool that gulped emissions
    /// * `new_backstop_emissions` - The amount of new emissions for the backstop
    /// * `new_pool_emissions` - The amount of new emissions for the pool
    pub fn gulp_emissions(
        e: &Env,
        pool_address: Address,
        new_backstop_emissions: i128,
        new_pool_emissions: i128,
    ) {
        let topics = (Symbol::new(e, "gulp_emissions"), pool_address);
        e.events()
            .publish(topics, (new_backstop_emissions, new_pool_emissions));
    }

    /// Emitted when the reward zone is updated
    ///
    /// - topics - `["rw_zone_add"]`
    /// - data - `[to_add: Address, to_remove: Address]`
    ///
    /// ### Arguments
    /// * `to_add` - The address to add to the reward zone
    /// * `to_remove` - The address to remove from the reward zone
    pub fn rw_zone_add(e: &Env, to_add: Address, to_remove: Option<Address>) {
        let topics = (Symbol::new(e, "rw_zone_add"),);
        e.events().publish(topics, (to_add, to_remove));
    }

    /// Emitted when a pool is removed from the reward zone
    ///
    /// - topics - `["rw_zone_remove", pool_address: Address]`
    /// - data - `[to_remove: Address]`
    ///
    /// ### Arguments
    /// * `to_remove` - The address to remove from the reward zone
    pub fn rw_zone_remove(e: &Env, to_remove: Address) {
        let topics = (Symbol::new(e, "rw_zone_remove"),);
        e.events().publish(topics, to_remove);
    }

    /// Emitted when emissions are claimed
    ///
    /// - topics - `["claim", from: Address]`
    /// - data - `[amount: i128]`
    ///
    /// ### Arguments
    /// * `from` - The address of the user claiming emissions
    /// * `amount` - The amount of LP tokens minted
    pub fn claim(e: &Env, from: Address, amount: i128) {
        let topics = (Symbol::new(e, "claim"), from);
        e.events().publish(topics, amount);
    }

    /// Emitted when tokens are drawn from the backstop
    ///
    /// - topics - `["draw", pool_address: Address]`
    /// - data - `[to: Address, amount: i128]`
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool
    /// * `to` - The address receiving the drawn tokens
    /// * `amount` - The amount of tokens drawn
    pub fn draw(e: &Env, pool_address: Address, to: Address, amount: i128) {
        let topics = (Symbol::new(e, "draw"), pool_address);
        e.events().publish(topics, (to, amount));
    }

    /// Emitted when tokens are donated to the backstop
    ///
    /// - topics - `["donate", pool_address: Address, from: Address]`
    /// - data - `[amount: i128]`
    ///
    /// ### Arguments
    /// * `pool_address` - The address of the pool
    /// * `from` - The address of the donor
    /// * `amount` - The amount of tokens donated
    pub fn donate(e: &Env, pool_address: Address, from: Address, amount: i128) {
        let topics = (Symbol::new(e, "donate"), pool_address, from);
        e.events().publish(topics, amount);
    }
}
