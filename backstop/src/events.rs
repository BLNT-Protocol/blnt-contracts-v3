use soroban_sdk::{contractevent, Address, Env};

use crate::backstop::BackstopTier;

macro_rules! single_value_event {
    (
        $name:ident,
        $topic:literal,
        [$($topic_field:ident: $topic_type:tt),* $(,)?],
        $data_field:ident: $data_type:tt
    ) => {
        #[contractevent(topics = [$topic], data_format = "single-value")]
        struct $name {
            $(
                #[topic]
                $topic_field: $topic_type,
            )*
            $data_field: $data_type,
        }
    };
}

macro_rules! vec_event {
    (
        $name:ident,
        $topic:literal,
        [$($topic_field:ident: $topic_type:tt),* $(,)?],
        [$($data_field:ident: $data_type:tt),+ $(,)?]
    ) => {
        #[contractevent(topics = [$topic], data_format = "vec")]
        struct $name {
            $(
                #[topic]
                $topic_field: $topic_type,
            )*
            $(
                $data_field: $data_type,
            )+
        }
    };
}

vec_event!(
    MigrationPreparedEvent,
    "migration_prepared",
    [],
    [
        original_unlock: u64,
        retry_count: u32,
        verified_queue_unlock: u64
    ]
);
vec_event!(
    MigrationActivatedEvent,
    "migration_activated",
    [],
    [activated_at: u64, backfill_end: u64]
);
single_value_event!(
    BackfillFundedEvent,
    "backfill_funded",
    [],
    amount: i128
);
vec_event!(
    TierDepositEvent,
    "deposit",
    [tier: BackstopTier, pool_address: Address, from: Address],
    [tokens_in: i128, shares_minted: i128]
);
vec_event!(
    TierQueueWithdrawalEvent,
    "queue_withdrawal",
    [tier: BackstopTier, pool_address: Address, from: Address],
    [amount: i128, expiration: u64]
);
single_value_event!(
    TierDequeueWithdrawalEvent,
    "dequeue_withdrawal",
    [tier: BackstopTier, pool_address: Address, from: Address],
    amount: i128
);
vec_event!(
    TierWithdrawEvent,
    "withdraw",
    [tier: BackstopTier, pool_address: Address, from: Address],
    [amount: i128, tokens_out: i128]
);
single_value_event!(
    DistributeEvent,
    "distribute",
    [],
    new_tokens_emitted: i128
);
vec_event!(
    GulpEmissionsEvent,
    "gulp_emissions",
    [pool_address: Address],
    [new_backstop_emissions: i128, new_pool_emissions: i128]
);
#[contractevent(topics = ["rw_zone_add"], data_format = "vec")]
struct RewardZoneAddEvent {
    to_add: Address,
    to_remove: Option<Address>,
}
single_value_event!(
    RewardZoneRemoveEvent,
    "rw_zone_remove",
    [],
    to_remove: Address
);
vec_event!(
    ClaimEvent,
    "claim",
    [tier: BackstopTier, user: Address, pool: Address],
    [blnd_amount: i128, lp_amount: i128, shares: i128]
);
vec_event!(
    DrawEvent,
    "draw",
    [tier: BackstopTier, pool_address: Address],
    [to: Address, amount: i128]
);
single_value_event!(
    DonateEvent,
    "donate",
    [tier: BackstopTier, pool_address: Address, from: Address],
    amount: i128
);

pub struct BackstopEvents {}

impl BackstopEvents {
    pub fn migration_prepared(
        e: &Env,
        original_unlock: u64,
        retry_count: u32,
        verified_queue_unlock: u64,
    ) {
        MigrationPreparedEvent {
            original_unlock,
            retry_count,
            verified_queue_unlock,
        }
        .publish(e);
    }

    pub fn migration_activated(e: &Env, activated_at: u64, backfill_end: u64) {
        MigrationActivatedEvent {
            activated_at,
            backfill_end,
        }
        .publish(e);
    }

    pub fn backfill_funded(e: &Env, amount: i128) {
        BackfillFundedEvent { amount }.publish(e);
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
        TierDepositEvent {
            tier,
            pool_address,
            from,
            tokens_in,
            shares_minted,
        }
        .publish(e);
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
        TierQueueWithdrawalEvent {
            tier,
            pool_address,
            from,
            amount,
            expiration,
        }
        .publish(e);
    }

    /// Emit a v2-shaped dequeue event scoped to a v3 tier.
    pub fn tier_dequeue_withdrawal(
        e: &Env,
        tier: BackstopTier,
        pool_address: Address,
        from: Address,
        amount: i128,
    ) {
        TierDequeueWithdrawalEvent {
            tier,
            pool_address,
            from,
            amount,
        }
        .publish(e);
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
        TierWithdrawEvent {
            tier,
            pool_address,
            from,
            amount,
            tokens_out,
        }
        .publish(e);
    }

    /// Emitted when new emissions are distributed
    /// - topics - `["distribute"]`
    /// - data - `[new_tokens_emitted: i128]`
    ///
    /// ### Arguments
    /// * `new_tokens_emitted` - The amount of new tokens emitted
    pub fn distribute(e: &Env, new_tokens_emitted: i128) {
        DistributeEvent { new_tokens_emitted }.publish(e);
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
        GulpEmissionsEvent {
            pool_address,
            new_backstop_emissions,
            new_pool_emissions,
        }
        .publish(e);
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
        RewardZoneAddEvent { to_add, to_remove }.publish(e);
    }

    /// Emitted when a pool is removed from the reward zone
    ///
    /// - topics - `["rw_zone_remove", pool_address: Address]`
    /// - data - `[to_remove: Address]`
    ///
    /// ### Arguments
    /// * `to_remove` - The address to remove from the reward zone
    pub fn rw_zone_remove(e: &Env, to_remove: Address) {
        RewardZoneRemoveEvent { to_remove }.publish(e);
    }

    /// Emitted when a user's ongoing BLND compounds into its originating tier.
    pub fn claim(
        e: &Env,
        tier: BackstopTier,
        user: Address,
        pool: Address,
        blnd_amount: i128,
        lp_amount: i128,
        shares: i128,
    ) {
        ClaimEvent {
            tier,
            user,
            pool,
            blnd_amount,
            lp_amount,
            shares,
        }
        .publish(e);
    }

    /// Emitted when tokens are drawn from the backstop
    ///
    /// - topics - `["draw", tier: BackstopTier, pool_address: Address]`
    /// - data - `[to: Address, amount: i128]`
    ///
    /// ### Arguments
    /// * `tier` - The tier whose token is drawn
    /// * `pool_address` - The address of the pool
    /// * `to` - The address receiving the drawn tokens
    /// * `amount` - The amount of tokens drawn
    pub fn draw(e: &Env, tier: BackstopTier, pool_address: Address, to: Address, amount: i128) {
        DrawEvent {
            tier,
            pool_address,
            to,
            amount,
        }
        .publish(e);
    }

    /// Emitted when tokens are donated to the backstop
    ///
    /// - topics - `["donate", tier: BackstopTier, pool_address: Address, from: Address]`
    /// - data - `[amount: i128]`
    ///
    /// ### Arguments
    /// * `tier` - The tier whose token is donated
    /// * `pool_address` - The address of the pool
    /// * `from` - The address of the donor
    /// * `amount` - The amount of tokens donated
    pub fn donate(e: &Env, tier: BackstopTier, pool_address: Address, from: Address, amount: i128) {
        DonateEvent {
            tier,
            pool_address,
            from,
            amount,
        }
        .publish(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::Address as _, xdr::ScVal, Event, FromVal, IntoVal, Symbol, Val,
        Vec as SorobanVec,
    };

    fn assert_legacy_shape(e: &Env, event: &impl Event, topics: SorobanVec<Val>, data: Val) {
        assert_eq!(event.topics(e), topics);
        assert_eq!(
            ScVal::from_val(e, &event.data(e)),
            ScVal::from_val(e, &data)
        );
    }

    #[test]
    fn typed_events_preserve_legacy_backstop_shapes() {
        let e = Env::default();
        let pool = Address::generate(&e);
        let user = Address::generate(&e);
        let removed = Address::generate(&e);

        assert_legacy_shape(
            &e,
            &TierDepositEvent {
                tier: BackstopTier::BlndUsdc,
                pool_address: pool.clone(),
                from: user.clone(),
                tokens_in: 50,
                shares_minted: 45,
            },
            (
                Symbol::new(&e, "deposit"),
                BackstopTier::BlndUsdc,
                pool.clone(),
                user.clone(),
            )
                .into_val(&e),
            (50_i128, 45_i128).into_val(&e),
        );

        assert_legacy_shape(
            &e,
            &RewardZoneAddEvent {
                to_add: pool.clone(),
                to_remove: Some(removed.clone()),
            },
            (Symbol::new(&e, "rw_zone_add"),).into_val(&e),
            (pool, Some(removed)).into_val(&e),
        );

        assert_legacy_shape(
            &e,
            &BackfillFundedEvent { amount: 100 },
            (Symbol::new(&e, "backfill_funded"),).into_val(&e),
            100_i128.into_val(&e),
        );
    }
}
