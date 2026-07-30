use soroban_sdk::{contractevent, Address, BytesN, Env, Vec};

use crate::{
    AuctionData, BadDebtAuctionData, BadDebtAuctionFill, InterestAuctionData, InterestAuctionFill,
    ReserveConfig,
};

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

macro_rules! unit_event {
    (
        $name:ident,
        $topic:literal,
        [$($topic_field:ident: $topic_type:tt),* $(,)?]
    ) => {
        #[contractevent(topics = [$topic], data_format = "single-value")]
        struct $name {
            $(
                #[topic]
                $topic_field: $topic_type,
            )*
        }
    };
}

single_value_event!(
    NewBadDebtAuctionEvent,
    "new_bad_debt_auction",
    [],
    auction: BadDebtAuctionData
);
#[contractevent(
    topics = ["delete_bad_debt_auction"],
    data_format = "single-value"
)]
struct DeleteBadDebtAuctionEvent {
    auction_id: BytesN<32>,
}
vec_event!(
    FillBadDebtAuctionEvent,
    "fill_bad_debt_auction",
    [],
    [
        filler: Address,
        percent: u32,
        fill: BadDebtAuctionFill
    ]
);
single_value_event!(
    NewInterestAuctionEvent,
    "new_interest_auction",
    [],
    auction: InterestAuctionData
);
#[contractevent(
    topics = ["delete_interest_auction"],
    data_format = "single-value"
)]
struct DeleteInterestAuctionEvent {
    auction_id: BytesN<32>,
}
vec_event!(
    FillInterestAuctionEvent,
    "fill_interest_auction",
    [],
    [
        filler: Address,
        percent: u32,
        fill: InterestAuctionFill
    ]
);
single_value_event!(
    SetAdminEvent,
    "set_admin",
    [admin: Address],
    new_admin: Address
);
vec_event!(
    UpdatePoolEvent,
    "update_pool",
    [admin: Address],
    [
        backstop_take_rate: u32,
        max_positions: u32,
        min_collateral: i128
    ]
);
vec_event!(
    QueueSetReserveEvent,
    "queue_set_reserve",
    [admin: Address],
    [asset: Address, metadata: ReserveConfig]
);
single_value_event!(
    CancelSetReserveEvent,
    "cancel_set_reserve",
    [admin: Address],
    asset: Address
);
vec_event!(
    SetReserveEvent,
    "set_reserve",
    [],
    [asset: Address, index: u32]
);
single_value_event!(
    SetStatusEvent,
    "set_status",
    [],
    new_status: u32
);
single_value_event!(
    SetStatusAdminEvent,
    "set_status",
    [admin: Address],
    pool_status: u32
);
vec_event!(
    ReserveEmissionUpdateEvent,
    "reserve_emission_update",
    [],
    [res_token_id: u32, eps: u64, expiration: u64]
);
single_value_event!(
    GulpEmissionsEvent,
    "gulp_emissions",
    [],
    emissions: i128
);
#[contractevent(topics = ["claim"], data_format = "vec")]
struct ClaimEvent {
    #[topic]
    from: Address,
    reserve_token_ids: Vec<u32>,
    amount_claimed: i128,
}
single_value_event!(
    BadDebtEvent,
    "bad_debt",
    [user: Address, asset: Address],
    d_tokens: i128
);
single_value_event!(
    DefaultedDebtEvent,
    "defaulted_debt",
    [asset: Address],
    d_tokens_burnt: i128
);
vec_event!(
    SupplyEvent,
    "supply",
    [asset: Address, from: Address],
    [tokens_in: i128, b_tokens_minted: i128]
);
vec_event!(
    WithdrawEvent,
    "withdraw",
    [asset: Address, from: Address],
    [tokens_out: i128, b_tokens_burnt: i128]
);
vec_event!(
    SupplyCollateralEvent,
    "supply_collateral",
    [asset: Address, from: Address],
    [tokens_in: i128, b_tokens_minted: i128]
);
vec_event!(
    WithdrawCollateralEvent,
    "withdraw_collateral",
    [asset: Address, from: Address],
    [tokens_out: i128, b_tokens_burnt: i128]
);
vec_event!(
    BorrowEvent,
    "borrow",
    [asset: Address, from: Address],
    [tokens_out: i128, d_tokens_minted: i128]
);
vec_event!(
    RepayEvent,
    "repay",
    [asset: Address, from: Address],
    [tokens_in: i128, d_tokens_burnt: i128]
);
vec_event!(
    FlashLoanEvent,
    "flash_loan",
    [asset: Address, from: Address, contract: Address],
    [tokens_out: i128, d_tokens_minted: i128]
);
single_value_event!(
    GulpEvent,
    "gulp",
    [asset: Address],
    token_delta: i128
);
vec_event!(
    NewAuctionEvent,
    "new_auction",
    [auction_type: u32, user: Address],
    [percent: u32, auction_data: AuctionData]
);
vec_event!(
    FillAuctionEvent,
    "fill_auction",
    [auction_type: u32, user: Address],
    [
        filler: Address,
        fill_percent: i128,
        filled_auction_data: AuctionData
    ]
);
unit_event!(
    DeleteAuctionEvent,
    "delete_auction",
    [auction_type: u32, user: Address]
);

pub struct PoolEvents {}

impl PoolEvents {
    pub fn new_bad_debt_auction(e: &Env, auction: BadDebtAuctionData) {
        NewBadDebtAuctionEvent { auction }.publish(e);
    }

    pub fn delete_bad_debt_auction(e: &Env, auction_id: BytesN<32>) {
        DeleteBadDebtAuctionEvent { auction_id }.publish(e);
    }

    pub fn fill_bad_debt_auction(e: &Env, filler: Address, percent: u32, fill: BadDebtAuctionFill) {
        FillBadDebtAuctionEvent {
            filler,
            percent,
            fill,
        }
        .publish(e);
    }

    pub fn new_interest_auction(e: &Env, auction: InterestAuctionData) {
        NewInterestAuctionEvent { auction }.publish(e);
    }

    pub fn delete_interest_auction(e: &Env, auction_id: BytesN<32>) {
        DeleteInterestAuctionEvent { auction_id }.publish(e);
    }

    pub fn fill_interest_auction(
        e: &Env,
        filler: Address,
        percent: u32,
        fill: InterestAuctionFill,
    ) {
        FillInterestAuctionEvent {
            filler,
            percent,
            fill,
        }
        .publish(e);
    }

    /// Emitted when a new admin is set for a pool
    ///
    /// - topics - `["set_admin", admin: Address]`
    /// - data - `new_admin: Address`
    ///
    /// ### Arguments
    /// * admin - The current admin of the pool
    /// * new_admin - The new admin of the pool
    pub fn set_admin(e: &Env, admin: Address, new_admin: Address) {
        SetAdminEvent { admin, new_admin }.publish(e);
    }

    /// Emitted when pool parameters are updated
    ///
    /// - topics - `["update_pool", admin: Address]`
    /// - data - `[backstop_take_rate: u32, max_positions: u32, min_collateral: i128]`
    ///
    /// ### Arguments
    /// * admin - The current admin of the pool
    /// * backstop_take_rate - The new backstop take rate
    /// * max_positions - The new maximum number of positions
    pub fn update_pool(
        e: &Env,
        admin: Address,
        backstop_take_rate: u32,
        max_positions: u32,
        min_collateral: i128,
    ) {
        UpdatePoolEvent {
            admin,
            backstop_take_rate,
            max_positions,
            min_collateral,
        }
        .publish(e);
    }

    /// Emitted when a new reserve configuration change is queued
    ///
    /// - topics - `["queue_set_reserve", admin: Address]`
    /// - data - `[asset: Address, metadata: ReserveMetadata]`
    ///
    /// ### Arguments
    /// * admin - The current admin of the pool
    /// * asset - The asset to change the reserve configuration of
    /// * metadata - The new reserve configuration
    pub fn queue_set_reserve(e: &Env, admin: Address, asset: Address, metadata: ReserveConfig) {
        QueueSetReserveEvent {
            admin,
            asset,
            metadata,
        }
        .publish(e);
    }

    /// Emitted when a queued reserve configuration change is cancelled
    ///
    /// - topics - `["cancel_set_reserve", admin: Address]`
    /// - data - `asset: Address`
    ///
    /// ### Arguments
    /// * admin - The current admin of the pool
    /// * asset - The asset to cancel the reserve configuration change of
    pub fn cancel_set_reserve(e: &Env, admin: Address, asset: Address) {
        CancelSetReserveEvent { admin, asset }.publish(e);
    }

    /// Emitted when a reserve configuration change is set
    ///
    /// - topics - `["set_reserve"]`
    /// - data - `[asset: Address, index: u32]`
    ///
    /// ### Arguments
    /// * asset - The asset to change the reserve configuration of
    /// * index - The reserve index
    pub fn set_reserve(e: &Env, asset: Address, index: u32) {
        SetReserveEvent { asset, index }.publish(e);
    }

    /// Emitted when pool status is updated (non-admin)
    ///
    /// - topics - `["set_status"]`
    /// - data - `new_status: PoolStatus`
    ///
    /// ### Arguments
    /// * new_status - The new pool status
    pub fn set_status(e: &Env, new_status: u32) {
        SetStatusEvent { new_status }.publish(e);
    }

    /// Emitted when pool status is updated by admin
    ///
    /// - topics - `["set_status", admin: Address]`
    /// - data - `pool_status: PoolStatus`
    ///
    /// ### Arguments
    /// * admin - The admin setting the pool status
    /// * pool_status - The new pool status
    pub fn set_status_admin(e: &Env, admin: Address, pool_status: u32) {
        SetStatusAdminEvent { admin, pool_status }.publish(e);
    }

    /// Emitted when reserve emissions are updated
    ///
    /// - topics - `["reserve_emission_update"]`
    /// - data - `[res_token_id: u32, eps: u64, expiration: u64]`
    ///
    /// ### Arguments
    /// * res_token_id - The reserve token ID
    /// * eps - The new emissions per second
    /// * expiration - The new expiration time
    pub fn reserve_emission_update(e: &Env, res_token_id: u32, eps: u64, expiration: u64) {
        ReserveEmissionUpdateEvent {
            res_token_id,
            eps,
            expiration,
        }
        .publish(e);
    }

    /// Emitted when emissions are gulped
    ///
    /// - topics - `["gulp_emissions"]`
    /// - data - `emissions: i128`
    ///
    /// ### Arguments
    /// * emissions - The amount of emissions gulped
    pub fn gulp_emissions(e: &Env, emissions: i128) {
        GulpEmissionsEvent { emissions }.publish(e);
    }

    /// Emitted when emissions are claimed
    ///
    /// - topics - `["claim", from: Address]`
    /// - data - `[reserve_token_ids: Vec<u32>, amount_claimed: i128]`
    ///
    /// ### Arguments
    /// * from - The address claiming the emissions
    /// * reserve_token_ids - The reserve token IDs claimed
    /// * amount_claimed - The amount claimed
    pub fn claim(e: &Env, from: Address, reserve_token_ids: Vec<u32>, amount_claimed: i128) {
        ClaimEvent {
            from,
            reserve_token_ids,
            amount_claimed,
        }
        .publish(e);
    }

    /// Emitted when bad debt is recorded
    ///
    /// - topics - `["bad_debt", user: Address, asset: Address]`
    /// - data - `[d_tokens: i128]`
    ///
    /// ### Arguments
    /// * user - The user with bad debt
    /// * asset - The asset with bad debt
    /// * d_tokens - The amount of bad debt
    pub fn bad_debt(e: &Env, user: Address, asset: Address, d_tokens: i128) {
        BadDebtEvent {
            user,
            asset,
            d_tokens,
        }
        .publish(e);
    }

    /// Emitted when bad debt is defaulted
    ///
    /// - topics - `["defaulted_debt", asset: Address]`
    /// - data - `[d_tokens_burnt: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset with defaulted debt
    /// * d_tokens_burnt - The amount of defaulted d_tokens
    pub fn defaulted_debt(e: &Env, asset: Address, d_tokens_burnt: i128) {
        DefaultedDebtEvent {
            asset,
            d_tokens_burnt,
        }
        .publish(e);
    }

    /// Emitted when tokens are supplied
    ///
    /// - topics - `["supply", asset: Address, from: Address]`
    /// - data - `[tokens_in: i128, b_tokens_minted: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * tokens_in - The amount of tokens sent to the pool
    /// * b_tokens_minted - The amount of b_tokens minted
    pub fn supply(e: &Env, asset: Address, from: Address, tokens_in: i128, b_tokens_minted: i128) {
        SupplyEvent {
            asset,
            from,
            tokens_in,
            b_tokens_minted,
        }
        .publish(e);
    }

    /// Emitted when tokens are withdrawn
    ///
    /// - topics - `["withdraw", asset: Address, from: Address]`
    /// - data - `[tokens_out: i128, b_tokens_burnt: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * tokens_out - The amount of tokens withdrawn from the pool
    /// * b_tokens_burnt - The amount of b_tokens burnt
    pub fn withdraw(
        e: &Env,
        asset: Address,
        from: Address,
        tokens_out: i128,
        b_tokens_burnt: i128,
    ) {
        WithdrawEvent {
            asset,
            from,
            tokens_out,
            b_tokens_burnt,
        }
        .publish(e);
    }

    /// Emitted when collateral is supplied
    ///
    /// - topics - `["supply_collateral", asset: Address, from: Address]`
    /// - data - `[tokens_in: i128, b_tokens_minted: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * tokens_in - The amount of tokens sent to the pool
    /// * b_tokens_minted - The amount of b_tokens minted
    pub fn supply_collateral(
        e: &Env,
        asset: Address,
        from: Address,
        tokens_in: i128,
        b_tokens_minted: i128,
    ) {
        SupplyCollateralEvent {
            asset,
            from,
            tokens_in,
            b_tokens_minted,
        }
        .publish(e);
    }

    /// Emitted when collateral is withdrawn
    ///
    /// - topics - `["withdraw_collateral", asset: Address, from: Address]`
    /// - data - `[tokens_out: i128, b_tokens_burnt: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * tokens_out - The amount of tokens withdrawn from the pool
    /// * b_tokens_burnt - The amount of b_tokens burnt
    pub fn withdraw_collateral(
        e: &Env,
        asset: Address,
        from: Address,
        tokens_out: i128,
        b_tokens_burnt: i128,
    ) {
        WithdrawCollateralEvent {
            asset,
            from,
            tokens_out,
            b_tokens_burnt,
        }
        .publish(e);
    }

    /// Emitted when tokens are borrowed
    ///
    /// - topics - `["borrow", asset: Address, from: Address]`
    /// - data - `[tokens_out: i128, d_tokens_minted: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * tokens_out - The amount of tokens sent from the pool
    /// * d_tokens_burnt - The amount of d_tokens burnt
    pub fn borrow(e: &Env, asset: Address, from: Address, tokens_out: i128, d_tokens_minted: i128) {
        BorrowEvent {
            asset,
            from,
            tokens_out,
            d_tokens_minted,
        }
        .publish(e);
    }

    /// Emitted when a loan is repaid
    ///
    /// - topics - `["repay", asset: Address, from: Address]`
    /// - data - `[tokens_in: i128, d_tokens_burnt: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * tokens_in - The amount of tokens sent to the pool
    /// * d_tokens_burnt - The amount of d_tokens burnt
    pub fn repay(e: &Env, asset: Address, from: Address, tokens_in: i128, d_tokens_burnt: i128) {
        RepayEvent {
            asset,
            from,
            tokens_in,
            d_tokens_burnt,
        }
        .publish(e);
    }

    /// Emitted during a flash loan
    ///
    /// - topics - `["flash_loan", asset: Address, from: Address]`
    /// - data - `[tokens_out: i128, d_tokens_minted: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * from - The address whose position is being modified
    /// * contract - The address of the flash loan contract
    /// * tokens_out - The amount of tokens sent from the pool
    /// * d_tokens_burnt - The amount of d_tokens burnt
    pub fn flash_loan(
        e: &Env,
        asset: Address,
        from: Address,
        contract: Address,
        tokens_out: i128,
        d_tokens_minted: i128,
    ) {
        FlashLoanEvent {
            asset,
            from,
            contract,
            tokens_out,
            d_tokens_minted,
        }
        .publish(e);
    }

    /// Emitted when a reserve gulps excess tokens
    ///
    /// - topics - `["gulp", asset: Address]`
    /// - data - `[token_delta: i128]`
    ///
    /// ### Arguments
    /// * asset - The asset
    /// * token_delta - The number of tokens gulped
    pub fn gulp(e: &Env, asset: Address, token_delta: i128) {
        GulpEvent { asset, token_delta }.publish(e);
    }

    /// Emitted when a new auction is created
    ///
    /// - topics - `["new_auction", auction_type: u32, user: Address]`
    /// - data - `[percent: u32, auction_data: AuctionData]`
    ///
    /// ### Arguments
    /// * auction_type - The type of auction
    /// * user - The auction user
    /// * percent - The percent of assets auctioned off
    /// * auction_data - The auction data
    pub fn new_auction(
        e: &Env,
        auction_type: u32,
        user: Address,
        percent: u32,
        auction_data: AuctionData,
    ) {
        NewAuctionEvent {
            auction_type,
            user,
            percent,
            auction_data,
        }
        .publish(e);
    }

    /// Emitted when an auction is filled
    ///
    /// - topics - `["fill_auction", auction_type: u32, user: Address]`
    /// - data - `[filler: Address, fill_percent: i128, filled_auction_data: AuctionData]`
    ///
    /// ### Arguments
    /// * auction_type - The type of auction
    /// * user - The auction user
    /// * filler - The address of the filler
    /// * fill_percent - The percentage of the auction filled
    /// * filled_auction_data - The filled auction data
    pub fn fill_auction(
        e: &Env,
        auction_type: u32,
        user: Address,
        filler: Address,
        fill_percent: i128,
        filled_auction_data: AuctionData,
    ) {
        FillAuctionEvent {
            auction_type,
            user,
            filler,
            fill_percent,
            filled_auction_data,
        }
        .publish(e);
    }

    /// Emitted when an auction is deleted
    ///
    /// - topics - `["delete_auction", auction_type: u32, user: Address]`
    /// - data - `()`
    ///
    /// ### Arguments
    /// * auction_type - The type of auction
    /// * user - The address of the user
    pub fn delete_auction(e: &Env, auction_type: u32, user: Address) {
        DeleteAuctionEvent { auction_type, user }.publish(e);
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
    fn typed_events_preserve_legacy_pool_shapes() {
        let e = Env::default();
        let asset = Address::generate(&e);
        let from = Address::generate(&e);

        assert_legacy_shape(
            &e,
            &SupplyEvent {
                asset: asset.clone(),
                from: from.clone(),
                tokens_in: 50,
                b_tokens_minted: 45,
            },
            (Symbol::new(&e, "supply"), asset.clone(), from.clone()).into_val(&e),
            (50_i128, 45_i128).into_val(&e),
        );

        assert_legacy_shape(
            &e,
            &SetStatusEvent { new_status: 3 },
            (Symbol::new(&e, "set_status"),).into_val(&e),
            3_u32.into_val(&e),
        );

        assert_legacy_shape(
            &e,
            &DeleteAuctionEvent {
                auction_type: 0,
                user: from.clone(),
            },
            (Symbol::new(&e, "delete_auction"), 0_u32, from).into_val(&e),
            ().into_val(&e),
        );
    }
}
